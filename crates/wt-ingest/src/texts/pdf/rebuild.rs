//! Rebuilding the text from page layouts (Spec 08, PDFs and OCR, step 3): lines joined into
//! paragraphs, hyphenation at line breaks removed, page numbers and running heads dropped,
//! small type at the foot of a page excluded as footnotes, larger type taken for section
//! headings, and every word kept with its page, confidence and box so a unit can be traced
//! back to where it stands.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::super::extract::{Section, normalize};
use super::layout::{Block, Line, Page, median};

/// A line in the top or bottom share of a page is in its margin.
const MARGIN: f32 = 0.10;
/// A block at or below this share of the page height can be a footnote.
const FOOT: f32 = 0.55;
/// Type this much smaller than the body is footnote type.
const FOOTNOTE_SIZE: f32 = 0.85;
/// Type this much larger than the body is a heading.
const HEADING_SIZE: f32 = 1.3;
/// A heading has at most this many words.
const HEADING_WORDS: usize = 12;
/// A margin line repeated on at least this many pages is a running head.
const RUNNING_HEAD_PAGES: usize = 3;

/// A word with where it came from.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Placed {
    /// Its source file, by index in the manifest.
    pub file: usize,
    /// Its normalized text.
    pub text: String,
    /// Its page.
    pub page: u32,
    /// Its confidence.
    pub conf: f32,
    /// Its box as shares of the page: left, top, right, bottom.
    pub frac: [f32; 4],
}

/// Something left out, for the dry run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Excluded {
    /// Its page.
    pub page: u32,
    /// `page_number`, `running_head` or `footnote`.
    pub kind: &'static str,
    /// Its text, up to 80 characters.
    pub text: String,
}

/// The rebuilt document.
#[derive(Debug, Clone, Default)]
pub struct Rebuilt {
    /// Sections as the other formats produce them.
    pub sections: Vec<Section>,
    /// Every kept word in reading order, headings excepted.
    pub words: Vec<Placed>,
    /// What was left out.
    pub excluded: Vec<Excluded>,
}

fn margin_key(line: &Line) -> String {
    line.text()
        .chars()
        .filter(|c| c.is_alphabetic())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_page_number(text: &str) -> bool {
    let t = text
        .trim()
        .trim_matches(|c| matches!(c, '[' | ']' | '(' | ')' | '-' | ' '));
    !t.is_empty()
        && ((t.len() <= 4 && t.chars().all(|c| c.is_ascii_digit()))
            || (t.len() <= 6 && t.chars().all(|c| "ivxlcdmIVXLCDM".contains(c))))
}

fn in_margin(page: &Page, line: &Line) -> bool {
    page.height > 0.0
        && (line.bottom() <= page.height * MARGIN || line.top() >= page.height * (1.0 - MARGIN))
}

fn short(text: &str) -> String {
    text.chars().take(80).collect()
}

/// Margin lines whose letters repeat on several pages: running heads, such as a book's title
/// on one side and its chapter on the other.
fn running_heads(pages: &[Page]) -> BTreeSet<String> {
    let mut seen: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    for page in pages {
        for block in &page.blocks {
            for line in &block.lines {
                let key = margin_key(line);
                if in_margin(page, line) && !key.is_empty() {
                    seen.entry(key).or_default().insert(page.number);
                }
            }
        }
    }
    seen.into_iter()
        .filter(|(_, p)| p.len() >= RUNNING_HEAD_PAGES)
        .map(|(k, _)| k)
        .collect()
}

fn placed(page: &Page, w: &super::layout::Word) -> Placed {
    let (pw, ph) = (page.width.max(1.0), page.height.max(1.0));
    Placed {
        file: 0,
        text: normalize(&w.text),
        page: page.number,
        conf: w.conf,
        frac: [w.x0 / pw, w.y0 / ph, w.x1 / pw, w.y1 / ph],
    }
}

/// Joins a block's lines into words, removing a hyphen at a line break when the next line
/// starts in lower case. `line_breaks` keeps a marker between lines for verse.
fn block_words(page: &Page, lines: &[&Line], line_breaks: bool) -> Vec<Vec<Placed>> {
    let mut out: Vec<Vec<Placed>> = Vec::new();
    for line in lines {
        let mut words: Vec<Placed> = line.words.iter().map(|w| placed(page, w)).collect();
        words.retain(|w| !w.text.is_empty());
        let Some(first) = words.first() else {
            continue;
        };
        let joins = out
            .last()
            .and_then(|prev| prev.last())
            .is_some_and(|last| hyphen_joins(&last.text, &first.text));
        if joins
            && let Some(prev) = out.last_mut()
            && let Some(last) = prev.last_mut()
        {
            let next = words.remove(0);
            last.text.pop();
            last.text.push_str(&next.text);
            last.conf = last.conf.min(next.conf);
            if words.is_empty() {
                continue;
            }
            if !line_breaks {
                prev.extend(words);
                continue;
            }
        } else if !line_breaks && let Some(prev) = out.last_mut() {
            prev.extend(words);
            continue;
        }
        out.push(words);
    }
    out
}

fn hyphen_joins(last: &str, next: &str) -> bool {
    let mut rev = last.chars().rev();
    rev.next() == Some('-')
        && rev.next().is_some_and(char::is_alphabetic)
        && next.chars().next().is_some_and(char::is_lowercase)
}

fn ends_sentence(word: &str) -> bool {
    word.trim_end_matches(['"', '\'', ')', ']'])
        .ends_with(['.', '!', '?', ':', ';'])
}

/// Rebuilds the text of `pages`. With `strip_running_heads`, repeated margin lines go. With
/// `line_breaks`, a block's lines stay separate lines, as verse needs.
pub fn rebuild(pages: &[Page], strip_running_heads: bool, line_breaks: bool) -> Rebuilt {
    let body = median(
        pages
            .iter()
            .flat_map(Page::words)
            .map(|w| w.height())
            .collect(),
    );
    let heads = if strip_running_heads {
        running_heads(pages)
    } else {
        BTreeSet::new()
    };
    let mut out = Rebuilt::default();
    let mut title: Option<String> = None;
    // Paragraphs of the current section: each a list of lines of placed words.
    let mut paragraphs: Vec<Vec<Vec<Placed>>> = Vec::new();
    let flush =
        |title: &mut Option<String>, paragraphs: &mut Vec<Vec<Vec<Placed>>>, out: &mut Rebuilt| {
            if paragraphs.is_empty() && title.is_none() {
                return;
            }
            let blocks: Vec<String> = paragraphs
                .iter()
                .map(|lines| {
                    lines
                        .iter()
                        .map(|l| {
                            l.iter()
                                .map(|w| w.text.as_str())
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .filter(|b| !b.trim().is_empty())
                .collect();
            out.words.extend(paragraphs.drain(..).flatten().flatten());
            if !blocks.is_empty() || title.is_some() {
                out.sections.push(Section {
                    title: title.take(),
                    blocks,
                });
            }
        };
    for page in pages {
        let mut kept: Vec<(&Block, Vec<&Line>)> = Vec::new();
        for block in &page.blocks {
            let mut lines = Vec::new();
            for line in &block.lines {
                if in_margin(page, line) && is_page_number(&line.text()) {
                    out.excluded.push(Excluded {
                        page: page.number,
                        kind: "page_number",
                        text: line.text(),
                    });
                } else if in_margin(page, line) && heads.contains(&margin_key(line)) {
                    out.excluded.push(Excluded {
                        page: page.number,
                        kind: "running_head",
                        text: short(&line.text()),
                    });
                } else {
                    lines.push(line);
                }
            }
            if !lines.is_empty() {
                kept.push((block, lines));
            }
        }
        // Footnotes: the run of small-type blocks that ends the page, below its upper part.
        let mut foot_from = kept.len();
        while foot_from > 0 {
            let (block, _) = kept[foot_from - 1];
            let small = body > 0.0 && block.type_size() < body * FOOTNOTE_SIZE;
            if small && page.height > 0.0 && block.top() >= page.height * FOOT {
                foot_from -= 1;
            } else {
                break;
            }
        }
        for (block, lines) in kept.drain(foot_from..) {
            let text: Vec<String> = lines.iter().map(|l| l.text()).collect();
            out.excluded.push(Excluded {
                page: page.number,
                kind: "footnote",
                text: short(&text.join(" ")),
            });
            let _ = block;
        }
        for (i, (block, lines)) in kept.into_iter().enumerate() {
            let words: usize = lines.iter().map(|l| l.words.len()).sum();
            if body > 0.0 && block.type_size() >= body * HEADING_SIZE && words <= HEADING_WORDS {
                flush(&mut title, &mut paragraphs, &mut out);
                let heading: Vec<String> = lines.iter().map(|l| l.text()).collect();
                title = Some(normalize(&heading.join(" ")));
                continue;
            }
            let mut para = block_words(page, &lines, line_breaks);
            if para.is_empty() {
                continue;
            }
            // A paragraph that runs over a page break continues the last one.
            let continues = i == 0
                && paragraphs
                    .last()
                    .and_then(|p| p.last())
                    .and_then(|l| l.last())
                    .is_some_and(|w| !ends_sentence(&w.text))
                && para[0]
                    .first()
                    .is_some_and(|w| w.text.chars().next().is_some_and(char::is_lowercase));
            if continues && let Some(prev) = paragraphs.last_mut() {
                let joins = prev
                    .last()
                    .and_then(|l| l.last())
                    .zip(para[0].first())
                    .is_some_and(|(a, b)| hyphen_joins(&a.text, &b.text));
                if joins
                    && let Some(last_line) = prev.last_mut()
                    && let Some(last) = last_line.last_mut()
                {
                    let next = para[0].remove(0);
                    last.text.pop();
                    last.text.push_str(&next.text);
                    last.conf = last.conf.min(next.conf);
                }
                if line_breaks {
                    prev.extend(para.into_iter().filter(|l| !l.is_empty()));
                } else if let Some(last_line) = prev.last_mut() {
                    last_line.extend(para.into_iter().flatten());
                }
                continue;
            }
            para.retain(|l| !l.is_empty());
            paragraphs.push(para);
        }
    }
    flush(&mut title, &mut paragraphs, &mut out);
    out
}

/// A key for matching a word: its letters and digits, lower case.
pub fn match_key(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Where a unit stands: its first page, its mean confidence and its box on that page.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Placement {
    /// Its source file, by index in the manifest.
    pub file: usize,
    /// The page it starts on.
    pub page: u32,
    /// The mean confidence of its words.
    pub conf: f32,
    /// The box around its words on its first page, as shares of the page.
    pub frac: [f32; 4],
}

/// Finds each unit's words in `words`, in order. A unit whose first words are not found gets
/// `None`.
pub fn place<'a>(
    units: impl IntoIterator<Item = &'a str>,
    words: &[Placed],
) -> Vec<Option<Placement>> {
    let stream: Vec<(String, &Placed)> = words
        .iter()
        .map(|w| (match_key(&w.text), w))
        .filter(|(k, _)| !k.is_empty())
        .collect();
    let mut cursor = 0;
    let mut out = Vec::new();
    for unit in units {
        let keys: Vec<String> = unit
            .split_whitespace()
            .map(match_key)
            .filter(|k| !k.is_empty())
            .collect();
        let probe = keys.len().min(4);
        let found = (probe > 0)
            .then(|| {
                (cursor..stream.len().saturating_sub(probe - 1))
                    .find(|&i| (0..probe).all(|j| stream[i + j].0 == keys[j]))
            })
            .flatten();
        let Some(start) = found else {
            out.push(None);
            continue;
        };
        let end = (start + keys.len()).min(stream.len());
        let span = &stream[start..end];
        let (file, page) = (span[0].1.file, span[0].1.page);
        let n = span.len().max(1);
        #[allow(clippy::cast_precision_loss)]
        let conf = span.iter().map(|(_, w)| w.conf).sum::<f32>() / n as f32;
        let mut frac = [f32::MAX, f32::MAX, 0.0_f32, 0.0_f32];
        for (_, w) in span
            .iter()
            .filter(|(_, w)| w.file == file && w.page == page)
        {
            frac[0] = frac[0].min(w.frac[0]);
            frac[1] = frac[1].min(w.frac[1]);
            frac[2] = frac[2].max(w.frac[2]);
            frac[3] = frac[3].max(w.frac[3]);
        }
        out.push(Some(Placement {
            file,
            page,
            conf,
            frac,
        }));
        cursor = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::layout::{Origin, Word};
    use super::*;

    fn word(text: &str, x: f32, y: f32, size: f32) -> Word {
        Word {
            text: text.into(),
            conf: 0.9,
            x0: x,
            y0: y,
            x1: x + 10.0 * text.len() as f32,
            y1: y + size,
        }
    }

    fn line(text: &str, y: f32, size: f32) -> Line {
        Line {
            words: text
                .split(' ')
                .enumerate()
                .map(|(i, t)| word(t, 72.0 + 60.0 * i as f32, y, size))
                .collect(),
        }
    }

    fn block(lines: &[&str], y: f32, size: f32) -> Block {
        Block {
            lines: lines
                .iter()
                .enumerate()
                .map(|(i, t)| line(t, y + i as f32 * size * 1.2, size))
                .collect(),
        }
    }

    fn page(number: u32, blocks: Vec<Block>) -> Page {
        Page {
            number,
            width: 612.0,
            height: 792.0,
            blocks,
            origin: Origin::TextLayer,
        }
    }

    fn book() -> Vec<Page> {
        (1..=4)
            .map(|n| {
                let mut blocks = vec![block(&["THE SYNTHETIC SUTRA"], 30.0, 9.0)];
                if n == 1 {
                    blocks.push(block(&["Opening"], 100.0, 18.0));
                }
                blocks.push(block(
                    &[
                        "Sitting in the morning the student practiced medi-",
                        "tation without any purpose at all.",
                    ],
                    200.0,
                    12.0,
                ));
                blocks.push(block(&["The bell rang and the hall fell"], 400.0, 12.0));
                blocks.push(block(&["1 A note in small type at the foot."], 700.0, 8.0));
                blocks.push(block(&[&format!("{n}")], 760.0, 10.0));
                page(n, blocks)
            })
            .collect()
    }

    #[test]
    fn drops_heads_numbers_and_footnotes() {
        let r = rebuild(&book(), true, false);
        let kinds: Vec<&str> = r.excluded.iter().map(|e| e.kind).collect();
        assert_eq!(kinds.iter().filter(|k| **k == "running_head").count(), 4);
        assert_eq!(kinds.iter().filter(|k| **k == "page_number").count(), 4);
        assert_eq!(kinds.iter().filter(|k| **k == "footnote").count(), 4);
        let all: String = r
            .sections
            .iter()
            .flat_map(|s| s.blocks.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!all.contains("SYNTHETIC"));
        assert!(!all.contains("small type"));
        assert!(all.contains("practiced meditation without"), "{all}");
        assert_eq!(r.sections[0].title.as_deref(), Some("Opening"));
    }

    #[test]
    fn keeps_running_heads_when_asked() {
        let r = rebuild(&book(), false, false);
        assert!(r.excluded.iter().all(|e| e.kind != "running_head"));
    }

    #[test]
    fn joins_a_paragraph_across_a_page_break() {
        let pages = vec![
            page(
                1,
                vec![block(&["The teacher said that the mind is"], 200.0, 12.0)],
            ),
            page(
                2,
                vec![block(&["like the moon on water.", ""], 100.0, 12.0)],
            ),
        ];
        let r = rebuild(&pages, true, false);
        assert_eq!(
            r.sections[0].blocks,
            vec!["The teacher said that the mind is like the moon on water."]
        );
    }

    #[test]
    fn keeps_line_breaks_for_verse() {
        let pages = vec![page(
            1,
            vec![block(&["First line here", "second line here"], 200.0, 12.0)],
        )];
        let r = rebuild(&pages, true, true);
        assert_eq!(
            r.sections[0].blocks,
            vec!["First line here\nsecond line here"]
        );
    }

    #[test]
    fn places_units_on_their_pages() {
        let r = rebuild(&book(), true, false);
        let units = [
            "The bell rang and the hall fell",
            "Sitting in the morning the student",
        ];
        let placed = place(units, &r.words);
        let first = placed[0].as_ref().unwrap();
        assert_eq!(first.page, 1);
        assert!((first.conf - 0.9).abs() < 1e-6);
        assert!(
            first.frac[1] > 0.4 && first.frac[3] < 0.6,
            "{:?}",
            first.frac
        );
        assert_eq!(placed[1].as_ref().unwrap().page, 2);
        assert!(place(["nothing like this"], &r.words)[0].is_none());
    }
}
