//! Rebuilding the text from page layouts (Spec 08, PDFs and OCR, step 3): lines joined into
//! paragraphs, hyphenation at line breaks removed, page numbers and running heads dropped,
//! small type at the foot of a page excluded as footnotes, larger type taken for section
//! headings, and every word kept with its page, confidence and box so a unit can be traced
//! back to where it stands.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::Serialize;

use super::super::extract::{Section, normalize};
use super::layout::{Line, Page, type_size};

/// A line in the top or bottom share of a page is in its margin.
const MARGIN: f32 = 0.10;
/// A page's first or last line set off from the rest by a gap of at least this many body type
/// sizes stands where a running head or page number stands, even below [`MARGIN`], as on a
/// journal scan with wide margins.
const EDGE_GAP: f32 = 1.0;
/// Margin lines whose letters differ in at most this share of places are one running head
/// read with OCR noise, such as "VIMALAKIRTI" and "VIMALAKINTI".
const HEAD_NOISE: f32 = 0.2;
/// Letters a margin line needs before it is matched with noise; shorter ones match exactly.
const HEAD_NOISE_MIN: usize = 8;
/// A block at or below this share of the page height can be a footnote.
const FOOT: f32 = 0.55;
/// Type this much smaller than the body is footnote type.
const FOOTNOTE_SIZE: f32 = 0.9;
/// A line starting this many body type sizes right of the page's text edge is indented.
const INDENT: f32 = 0.8;
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
    /// `page_number`, `running_head`, `footnote` or `noise`.
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

/// A margin line's letters in lower case, which leaves out its page number.
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

/// The lines of a page, by block and line index, that stand where running heads and page
/// numbers stand: those in its margins, and its first and last line when a gap of
/// [`EDGE_GAP`] sets them off from the text and they are not in heading type.
fn edge_lines(page: &Page, body: f32) -> HashSet<(usize, usize)> {
    let mut lines: Vec<(usize, usize, &Line)> = page
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(b, block)| {
            block
                .lines
                .iter()
                .enumerate()
                .map(move |(l, line)| (b, l, line))
        })
        .collect();
    lines.sort_by(|a, b| a.2.top().total_cmp(&b.2.top()));
    let mut out: HashSet<(usize, usize)> = lines
        .iter()
        .filter(|(_, _, line)| in_margin(page, line))
        .map(|&(b, l, _)| (b, l))
        .collect();
    let set_off = |line: &Line, gap: f32| {
        body > 0.0 && gap >= body * EDGE_GAP && type_size(line.words.iter()) < body * HEADING_SIZE
    };
    if let [first, second, ..] = lines.as_slice()
        && set_off(first.2, second.2.top() - first.2.bottom())
    {
        out.insert((first.0, first.1));
    }
    if let [.., before, last] = lines.as_slice()
        && set_off(last.2, last.2.top() - before.2.bottom())
    {
        out.insert((last.0, last.1));
    }
    out
}

fn short(text: &str) -> String {
    text.chars().take(80).collect()
}

/// Whether two margin keys are one running head, allowing for OCR noise.
fn same_head(a: &str, b: &str) -> bool {
    let n = a.chars().count().max(b.chars().count());
    #[allow(clippy::cast_precision_loss)]
    let noise = |d: usize| d as f32 <= n as f32 * HEAD_NOISE;
    a == b || (n >= HEAD_NOISE_MIN && noise(wt_registry::glossary::damerau_levenshtein(a, b)))
}

/// Edge lines whose letters repeat on several pages: running heads, such as a journal's name
/// on the left-hand pages and the article's title on the right-hand ones. Page numbers are left
/// out of the comparison, so a head matches whichever side its number stands on, and spellings
/// that differ only by OCR noise count as one head.
fn running_heads(pages: &[Page], edges: &[HashSet<(usize, usize)>]) -> BTreeSet<String> {
    let mut seen: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    for (page, edge) in pages.iter().zip(edges) {
        for &(b, l) in edge {
            let key = margin_key(&page.blocks[b].lines[l]);
            if !key.is_empty() {
                seen.entry(key).or_default().insert(page.number);
            }
        }
    }
    let mut keys: Vec<(String, BTreeSet<u32>)> = seen.into_iter().collect();
    keys.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    // Each group: its most frequent spelling, every spelling in it, and the pages it is on.
    let mut groups: Vec<(String, Vec<String>, BTreeSet<u32>)> = Vec::new();
    for (key, on) in keys {
        match groups.iter_mut().find(|g| same_head(&g.0, &key)) {
            Some(group) => {
                group.1.push(key);
                group.2.extend(on);
            }
            None => groups.push((key.clone(), vec![key], on)),
        }
    }
    groups
        .into_iter()
        .filter(|g| g.2.len() >= RUNNING_HEAD_PAGES)
        .flat_map(|g| g.1)
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

/// Whether a paragraph ending in `word` is complete. Quotes are straight by now, and a
/// footnote mark after the stop does not count.
fn ends_sentence(word: &str) -> bool {
    word.trim_end_matches(['"', '\'', ')', ']', '*', '\u{2020}', '\u{2021}'])
        .ends_with(['.', '!', '?', ':', ';'])
}

/// Rebuilds the text of `pages`. With `strip_running_heads`, repeated margin lines go. With
/// `line_breaks`, a block's lines stay separate lines, as verse needs.
pub fn rebuild(pages: &[Page], strip_running_heads: bool, line_breaks: bool) -> Rebuilt {
    let body = type_size(pages.iter().flat_map(Page::words));
    let edges: Vec<HashSet<(usize, usize)>> = pages.iter().map(|p| edge_lines(p, body)).collect();
    let heads = if strip_running_heads {
        running_heads(pages, &edges)
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
    for (page, edge) in pages.iter().zip(&edges) {
        // Each kept block, with whether only noise stood between it and the block before.
        let mut kept: Vec<(bool, Vec<&Line>)> = Vec::new();
        let mut after_noise = false;
        for (b, block) in page.blocks.iter().enumerate() {
            let mut lines = Vec::new();
            for (l, line) in block.lines.iter().enumerate() {
                let at_edge = edge.contains(&(b, l));
                if at_edge && is_page_number(&line.text()) {
                    out.excluded.push(Excluded {
                        page: page.number,
                        kind: "page_number",
                        text: line.text(),
                    });
                } else if at_edge && heads.contains(&margin_key(line)) {
                    out.excluded.push(Excluded {
                        page: page.number,
                        kind: "running_head",
                        text: short(&line.text()),
                    });
                } else {
                    lines.push(line);
                }
            }
            // Noise: a block with no letter or digit, such as a speck Tesseract reads as "?".
            // It goes, and the text on either side of it is read as if it were not there.
            let noise = !lines.is_empty()
                && !lines
                    .iter()
                    .flat_map(|l| l.words.iter())
                    .any(|w| w.text.chars().any(char::is_alphanumeric));
            if noise {
                let text: Vec<String> = lines.iter().map(|l| l.text()).collect();
                out.excluded.push(Excluded {
                    page: page.number,
                    kind: "noise",
                    text: short(&text.join(" ")),
                });
                after_noise = true;
            } else if !lines.is_empty() {
                kept.push((after_noise, lines));
                after_noise = false;
            }
        }
        let size = |lines: &[&Line]| type_size(lines.iter().flat_map(|l| l.words.iter()));
        let top = |lines: &[&Line]| lines.iter().map(|l| l.top()).fold(f32::MAX, f32::min);
        // Footnotes: small-type blocks in the lower part of the page with no body type below
        // them, wherever the tool put them in reading order.
        let small = |lines: &[&Line]| body > 0.0 && size(lines) < body * FOOTNOTE_SIZE;
        let body_bottom = kept
            .iter()
            .filter(|(_, lines)| !small(lines))
            .map(|(_, lines)| top(lines))
            .fold(f32::MIN, f32::max);
        let (feet, kept): (Vec<_>, Vec<_>) = kept.into_iter().partition(|(_, lines)| {
            small(lines)
                && page.height > 0.0
                && top(lines) >= page.height * FOOT
                && top(lines) > body_bottom
        });
        for (_, lines) in feet {
            let text: Vec<String> = lines.iter().map(|l| l.text()).collect();
            out.excluded.push(Excluded {
                page: page.number,
                kind: "footnote",
                text: short(&text.join(" ")),
            });
        }
        // The page's text edge, against which an indented first line starts a paragraph: the
        // lower quartile of the lines' left edges, since most lines are flush and a stray
        // speck can stand further left.
        let mut lefts: Vec<f32> = kept
            .iter()
            .flat_map(|(_, lines)| lines)
            .map(|l| l.left())
            .collect();
        lefts.sort_by(f32::total_cmp);
        let text_left = lefts.get(lefts.len() / 4).copied().unwrap_or(0.0);
        let indented = |line: &Line| body > 0.0 && line.left() - text_left > body * INDENT;
        for (i, (after_noise, lines)) in kept.into_iter().enumerate() {
            let words: usize = lines.iter().map(|l| l.words.len()).sum();
            if body > 0.0 && size(&lines) >= body * HEADING_SIZE && words <= HEADING_WORDS {
                flush(&mut title, &mut paragraphs, &mut out);
                let heading: Vec<String> = lines.iter().map(|l| l.text()).collect();
                title = Some(normalize(&heading.join(" ")));
                continue;
            }
            let mut para = block_words(page, &lines, line_breaks);
            if para.is_empty() {
                continue;
            }
            // A paragraph that runs over a page break, or past noise, continues the last one:
            // the last does not end a sentence and this block's first line is not indented.
            // Verse, whose lines seldom end in a stop, also needs the line to start in lower
            // case.
            let continues = (i == 0 || after_noise)
                && paragraphs
                    .last()
                    .and_then(|p| p.last())
                    .and_then(|l| l.last())
                    .is_some_and(|w| !ends_sentence(&w.text))
                && !lines.first().is_some_and(|l| indented(l))
                && (!line_breaks
                    || para[0]
                        .first()
                        .is_some_and(|w| w.text.chars().next().is_some_and(char::is_lowercase)));
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
    use super::super::layout::{Block, Origin, Word};
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

    /// A word as Tesseract boxes it at 300 dpi: as tall as its letters reach, so `size` is the
    /// ascender height and a word of x-height letters is shorter.
    fn ocr_word(text: &str, x: f32, y: f32, size: f32) -> Word {
        let rises = text
            .chars()
            .any(|c| c.is_ascii_digit() || c.is_uppercase() || "bdfhklt".contains(c));
        let falls = text.chars().any(|c| "gjpqy,;()".contains(c));
        let (top, height) = match (rises, falls) {
            (true, true) => (0.0, size * 1.27),
            (true, false) => (0.0, size),
            (false, true) => (size * 0.3, size * 0.97),
            (false, false) => (size * 0.3, size * 0.7),
        };
        #[allow(clippy::cast_precision_loss)]
        let width = size * 0.55 * text.chars().count() as f32;
        Word {
            text: text.into(),
            conf: 0.9,
            x0: x,
            y0: y + top,
            x1: x + width,
            y1: y + top + height,
        }
    }

    fn ocr_line(text: &str, x: f32, y: f32, size: f32) -> Line {
        let mut at = x;
        let words = text
            .split(' ')
            .map(|t| {
                let w = ocr_word(t, at, y, size);
                at = w.x1 + size * 0.4;
                w
            })
            .collect();
        Line { words }
    }

    /// A block whose lines start at `left`; a line given with a leading `>` is indented.
    fn ocr_block(lines: &[&str], left: f32, y: f32, size: f32) -> Block {
        Block {
            lines: lines
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let (x, t) = match t.strip_prefix('>') {
                        Some(rest) => (left + 88.0, rest),
                        None => (left, *t),
                    };
                    #[allow(clippy::cast_precision_loss)]
                    let y = y + i as f32 * size * 1.8;
                    ocr_line(t, x, y, size)
                })
                .collect(),
        }
    }

    /// A synthetic journal scan as Tesseract reads it: wide margins, so the running heads stand
    /// at 14 percent of the page height, below [`MARGIN`]; the journal's name on the even pages
    /// and the article's title on the odd ones, each with its page number on the outer side and
    /// some OCR noise; a footnote in smaller type at the foot of the second page that Tesseract
    /// lists first; and indented paragraphs, one of which runs over a page break.
    fn journal() -> Vec<Page> {
        let heads = [
            "THE LOTUS SUTRA 225",
            "226 THE SYNTHETIC REVIEW",
            "THE LOTUS SUTRE 227",
            "22.8 THE SYNTHETIC REVIEW",
            "THE L0TUS SUTRA 229",
            "23O THE SYNTHETIC REVIEW",
        ];
        let bodies: [&[&[&str]]; 6] = [
            &[
                &[
                    ">Then the teacher said to the student: Go thou to",
                    "the hall and ask after the health of the old monk.",
                ],
                &[
                    ">The student replied that the moon is one and",
                    "not divisible nor is",
                ],
            ],
            &[&[
                "Suchness divided into parts. The student",
                "then sat down and the bell rang",
            ]],
            &[&[
                ">Morning came and the student rose early.",
                "He swept the hall and lit the lamps.",
            ]],
            &[&[">The old monk smiled at him.", "Nothing more was said."]],
            &[&[">Then the rain stopped.", "The student went home."]],
            &[&[">The book ends here.", "Thus have I heard."]],
        ];
        heads
            .iter()
            .zip(bodies)
            .enumerate()
            .map(|(i, (head, body))| {
                // Verso pages sit further right on the scan, as a bound volume's do.
                let left = if i % 2 == 0 { 260.0 } else { 320.0 };
                let head_x = if i % 2 == 0 { 700.0 } else { left };
                let mut blocks = Vec::new();
                if i == 1 {
                    blocks.push(ocr_block(
                        &[
                            ">1 The moon here stands for the mind of",
                            "the student, as the old commentaries hold.",
                        ],
                        left,
                        2100.0,
                        25.0,
                    ));
                }
                blocks.push(ocr_block(&[head], head_x, 390.0, 23.0));
                let mut y = 470.0;
                for lines in body {
                    blocks.push(ocr_block(lines, left, y, 30.0));
                    #[allow(clippy::cast_precision_loss)]
                    let height = lines.len() as f32 * 54.0;
                    y += height + 20.0;
                }
                Page {
                    number: u32::try_from(i + 1).unwrap(),
                    width: 1830.0,
                    height: 2774.0,
                    blocks,
                    origin: Origin::Ocr,
                }
            })
            .collect()
    }

    fn all_text(r: &Rebuilt) -> Vec<String> {
        r.sections.iter().flat_map(|s| s.blocks.clone()).collect()
    }

    #[test]
    fn strips_alternating_heads_below_the_margin() {
        let r = rebuild(&journal(), true, false);
        let heads: Vec<&str> = r
            .excluded
            .iter()
            .filter(|e| e.kind == "running_head")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(heads.len(), 6, "{:?}", r.excluded);
        let all = all_text(&r).join("\n");
        for gone in ["LOTUS", "L0TUS", "SYNTHETIC", "226", "229"] {
            assert!(!all.contains(gone), "{gone} in {all}");
        }
        let r = rebuild(&journal(), false, false);
        assert!(all_text(&r).join("\n").contains("THE SYNTHETIC REVIEW"));
    }

    #[test]
    fn keeps_a_repeated_first_line_that_is_not_set_off() {
        // The same sentence opens three pages, but at the body's own spacing, so it is text.
        let pages: Vec<Page> = (1..=3)
            .map(|n| Page {
                number: n,
                width: 1830.0,
                height: 2774.0,
                blocks: vec![ocr_block(
                    &[
                        "Therefore I am not worthy to go.",
                        ">The teacher then asked another student.",
                    ],
                    260.0,
                    470.0,
                    30.0,
                )],
                origin: Origin::Ocr,
            })
            .collect();
        let r = rebuild(&pages, true, false);
        assert!(r.excluded.is_empty(), "{:?}", r.excluded);
        assert_eq!(all_text(&r).join(" ").matches("not worthy").count(), 3);
    }

    #[test]
    fn excludes_a_small_foot_block_listed_first() {
        let r = rebuild(&journal(), true, false);
        let feet: Vec<&Excluded> = r.excluded.iter().filter(|e| e.kind == "footnote").collect();
        assert_eq!(feet.len(), 1, "{:?}", r.excluded);
        assert_eq!(feet[0].page, 2);
        assert!(
            feet[0].text.starts_with("1 The moon here"),
            "{}",
            feet[0].text
        );
        assert!(!all_text(&r).join("\n").contains("commentaries"));
    }

    #[test]
    fn small_type_above_the_body_text_is_not_a_footnote() {
        let mut pages = journal();
        pages[1].blocks.push(ocr_block(
            &["A last line in body type."],
            320.0,
            2400.0,
            30.0,
        ));
        let r = rebuild(&pages, true, false);
        assert!(r.excluded.iter().all(|e| e.kind != "footnote"));
        assert!(all_text(&r).join("\n").contains("commentaries"));
    }

    #[test]
    fn joins_across_a_page_break_unless_the_next_line_is_indented() {
        let r = rebuild(&journal(), true, false);
        let blocks = all_text(&r);
        assert!(
            blocks.contains(
                &"The student replied that the moon is one and not divisible nor is Suchness \
                  divided into parts. The student then sat down and the bell rang"
                    .to_owned()
            ),
            "{blocks:#?}"
        );
        assert!(
            blocks
                .iter()
                .any(|b| b.starts_with("Morning came and the student rose early.")),
            "{blocks:#?}"
        );
        assert_eq!(blocks.len(), 6, "{blocks:#?}");
    }

    #[test]
    fn running_heads_match_through_ocr_noise() {
        assert!(same_head("thevimalakirtisutra", "thevimalakintisutra"));
        assert!(same_head("thevimalakirtisutra", "thevimalaririttsutra"));
        assert!(!same_head("theeasternbuddhist", "thevimalakirtisutra"));
        // Short lines must match exactly.
        assert!(!same_head("notes", "note"));
        assert!(same_head("notes", "notes"));
    }

    /// One page as Tesseract read a journal page: a paragraph split into two blocks by a speck
    /// read as "?", which stands level with the second block's first line.
    fn speck_page(second_first_line: &str) -> Page {
        let mut speck = ocr_block(&["?"], 746.0, 824.0, 11.0);
        speck.lines[0].words[0].y0 = 824.0;
        Page {
            number: 1,
            width: 1830.0,
            height: 2774.0,
            blocks: vec![
                ocr_block(
                    &[
                        ">The teacher asked: \"What is the meaning of emptiness?\"",
                        "The student replied: \"It is empty",
                    ],
                    342.0,
                    716.0,
                    30.0,
                ),
                speck,
                ocr_block(
                    &[second_first_line, "And the teacher said nothing more."],
                    342.0,
                    824.0,
                    30.0,
                ),
            ],
            origin: Origin::Ocr,
        }
    }

    #[test]
    fn a_speck_between_blocks_is_noise_and_the_paragraph_goes_on() {
        let r = rebuild(
            &[speck_page("because it is empty.\" Then he bowed.")],
            true,
            false,
        );
        assert_eq!(
            all_text(&r),
            vec![
                "The teacher asked: \"What is the meaning of emptiness?\" The student replied: \
                 \"It is empty because it is empty.\" Then he bowed. And the teacher said \
                 nothing more."
            ]
        );
        assert_eq!(
            r.excluded,
            vec![Excluded {
                page: 1,
                kind: "noise",
                text: "?".into()
            }]
        );
        assert!(r.words.iter().all(|w| w.text != "?"));
    }

    #[test]
    fn past_a_speck_an_indented_line_still_starts_a_paragraph() {
        let r = rebuild(
            &[speck_page(">Because it is empty, he bowed.")],
            true,
            false,
        );
        assert_eq!(all_text(&r).len(), 2, "{:?}", all_text(&r));
        assert_eq!(r.excluded.len(), 1);
    }

    #[test]
    fn without_a_speck_blocks_on_one_page_stay_apart() {
        let mut page = speck_page("because it is empty.\" Then he bowed.");
        page.blocks.remove(1);
        let r = rebuild(&[page], true, false);
        assert_eq!(all_text(&r).len(), 2, "{:?}", all_text(&r));
        assert!(r.excluded.is_empty());
    }

    #[test]
    fn type_size_follows_the_tall_words() {
        let line = ocr_line("one man saw a mountain", 0.0, 0.0, 30.0);
        assert!((type_size(line.words.iter()) - 30.0).abs() < 1e-3);
        let low = ocr_line("one man saw", 0.0, 0.0, 30.0);
        assert!((type_size(low.words.iter()) - 21.0).abs() < 1e-3);
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
