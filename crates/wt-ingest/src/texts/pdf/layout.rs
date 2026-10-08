//! Page layout as the tools report it (Spec 08, PDFs and OCR, steps 1 and 2): words with a
//! box and a confidence, grouped into lines and blocks, read from `pdftotext -bbox-layout` for
//! a text layer and from Tesseract's TSV for a scan.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::super::TextError;
use super::super::extract::decode_entities;

/// Where a page's words came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// The PDF's own text layer.
    TextLayer,
    /// Tesseract.
    Ocr,
}

/// One word.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    /// Its text.
    pub text: String,
    /// From 0 to 1; a text layer's words are 1.
    pub conf: f32,
    /// Left edge.
    pub x0: f32,
    /// Top edge.
    pub y0: f32,
    /// Right edge.
    pub x1: f32,
    /// Bottom edge.
    pub y1: f32,
}

impl Word {
    /// Its height, which stands in for the type size.
    pub fn height(&self) -> f32 {
        (self.y1 - self.y0).max(0.0)
    }
}

/// One line of words, left to right.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Line {
    /// Its words.
    pub words: Vec<Word>,
}

impl Line {
    /// Its words joined by spaces.
    pub fn text(&self) -> String {
        self.words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Its top edge.
    pub fn top(&self) -> f32 {
        self.words.iter().map(|w| w.y0).fold(f32::MAX, f32::min)
    }

    /// Its bottom edge.
    pub fn bottom(&self) -> f32 {
        self.words.iter().map(|w| w.y1).fold(0.0, f32::max)
    }

    /// Its left edge.
    pub fn left(&self) -> f32 {
        self.words.iter().map(|w| w.x0).fold(f32::MAX, f32::min)
    }
}

/// A block, which both tools take for a paragraph.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Block {
    /// Its lines, top to bottom.
    pub lines: Vec<Line>,
}

impl Block {
    /// Every word in reading order.
    pub fn words(&self) -> impl Iterator<Item = &Word> {
        self.lines.iter().flat_map(|l| l.words.iter())
    }

    /// Its top edge.
    pub fn top(&self) -> f32 {
        self.lines.iter().map(Line::top).fold(f32::MAX, f32::min)
    }

    /// Its type size: see [`type_size`].
    pub fn type_size(&self) -> f32 {
        type_size(self.words())
    }
}

/// Whether a word's box spans exactly the ascender height: letters with an ascender or a
/// capital, none with a descender, and no punctuation that reaches below the baseline.
fn ascender_word(text: &str) -> bool {
    let mut letters = text.chars().filter(|c| c.is_alphabetic()).peekable();
    letters.peek().is_some()
        && text
            .chars()
            .all(|c| c.is_alphabetic() || ".:!?'\"\u{2018}\u{2019}\u{201C}\u{201D}".contains(c))
        && text
            .chars()
            .filter(|c| c.is_alphabetic())
            .any(|c| c.is_uppercase() || "bdfhklt".contains(c))
        && !letters.any(|c| "gjpqyJQ".contains(c))
}

/// The type size of some words: the median height of those that span the ascender height, or
/// of all of them when none do. A word's box grows with its ascenders and descenders, so a
/// plain median of all words drifts with the wording; Tesseract's boxes for a footnote in 9
/// point type under 11 point body type came within a few percent of the body that way.
pub fn type_size<'a>(words: impl Iterator<Item = &'a Word>) -> f32 {
    let (tall, all): (Vec<&Word>, Vec<&Word>) = words.partition(|w| ascender_word(&w.text));
    let pick = if tall.is_empty() { all } else { tall };
    median(pick.into_iter().map(Word::height).collect())
}

/// One page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page {
    /// From 1.
    pub number: u32,
    /// Its width, in the tool's units.
    pub width: f32,
    /// Its height.
    pub height: f32,
    /// Its blocks in reading order.
    pub blocks: Vec<Block>,
    /// Where its words came from.
    pub origin: Origin,
}

impl Page {
    /// Every word.
    pub fn words(&self) -> impl Iterator<Item = &Word> {
        self.blocks.iter().flat_map(Block::words)
    }

    /// The characters on the page, without spaces.
    pub fn chars(&self) -> usize {
        self.words().map(|w| w.text.chars().count()).sum()
    }

    /// The mean word confidence, or `None` without words.
    pub fn mean_confidence(&self) -> Option<f32> {
        mean(self.words().map(|w| w.conf))
    }
}

/// The median, or 0 for nothing.
pub fn median(mut v: Vec<f32>) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    v[v.len() / 2]
}

/// The mean, or `None` for nothing.
pub fn mean(values: impl Iterator<Item = f32>) -> Option<f32> {
    let (sum, n) = values.fold((0.0_f64, 0_u32), |(s, n), v| (s + f64::from(v), n + 1));
    #[allow(clippy::cast_possible_truncation)]
    (n > 0).then(|| (sum / f64::from(n)) as f32)
}

static TAG: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"<(page|block|line)\b([^>]*)>|<word\b([^>]*)>([^<]*)</word>"#).ok()
});
static ATTR: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r#"(\w+)="([^"]*)""#).ok());

fn attr(attrs: &str, name: &str) -> f32 {
    ATTR.as_ref()
        .and_then(|re| {
            re.captures_iter(attrs)
                .find(|c| &c[1] == name)
                .and_then(|c| c[2].parse().ok())
        })
        .unwrap_or(0.0)
}

/// Reads the XHTML `pdftotext -bbox-layout` writes for one page. `number` is the page's number
/// in the PDF.
pub fn from_bbox_layout(xhtml: &str, number: u32) -> Result<Page, TextError> {
    let (Some(tag), Some(_)) = (TAG.as_ref(), ATTR.as_ref()) else {
        return Err(TextError::Source(
            "layout patterns failed to compile".into(),
        ));
    };
    let mut page = Page {
        number,
        width: 0.0,
        height: 0.0,
        blocks: Vec::new(),
        origin: Origin::TextLayer,
    };
    for c in tag.captures_iter(xhtml) {
        match (c.get(1).map(|m| m.as_str()), c.get(2), c.get(3), c.get(4)) {
            (Some("page"), Some(a), _, _) => {
                page.width = attr(a.as_str(), "width");
                page.height = attr(a.as_str(), "height");
            }
            (Some("block"), _, _, _) => page.blocks.push(Block::default()),
            (Some("line"), _, _, _) => {
                if page.blocks.is_empty() {
                    page.blocks.push(Block::default());
                }
                if let Some(b) = page.blocks.last_mut() {
                    b.lines.push(Line::default());
                }
            }
            (None, _, Some(a), Some(text)) => {
                let text = decode_entities(text.as_str()).trim().to_owned();
                if text.is_empty() {
                    continue;
                }
                let a = a.as_str();
                let word = Word {
                    text,
                    conf: 1.0,
                    x0: attr(a, "xMin"),
                    y0: attr(a, "yMin"),
                    x1: attr(a, "xMax"),
                    y1: attr(a, "yMax"),
                };
                if page.blocks.is_empty() {
                    page.blocks.push(Block::default());
                }
                if let Some(b) = page.blocks.last_mut() {
                    if b.lines.is_empty() {
                        b.lines.push(Line::default());
                    }
                    if let Some(l) = b.lines.last_mut() {
                        l.words.push(word);
                    }
                }
            }
            _ => {}
        }
    }
    for b in &mut page.blocks {
        b.lines.retain(|l| !l.words.is_empty());
    }
    page.blocks.retain(|b| !b.lines.is_empty());
    Ok(page)
}

/// Reads Tesseract's TSV for one page image. Each paragraph becomes a block; confidences go
/// from Tesseract's 0 to 100 to 0 to 1.
pub fn from_tesseract_tsv(tsv: &str, number: u32) -> Result<Page, TextError> {
    let mut page = Page {
        number,
        width: 0.0,
        height: 0.0,
        blocks: Vec::new(),
        origin: Origin::Ocr,
    };
    let mut current: Option<(u32, u32)> = None;
    let mut current_line: Option<u32> = None;
    for (i, row) in tsv.lines().enumerate() {
        if i == 0 && row.starts_with("level") {
            continue;
        }
        let cols: Vec<&str> = row.split('\t').collect();
        if cols.len() < 12 {
            continue;
        }
        let n = |j: usize| -> Result<f32, TextError> {
            cols[j].trim().parse::<f32>().map_err(|e| {
                TextError::Source(format!("Tesseract TSV row {}: column {j}: {e}", i + 1))
            })
        };
        let level = n(0)?;
        let (left, top, width, height) = (n(6)?, n(7)?, n(8)?, n(9)?);
        #[allow(clippy::float_cmp)]
        if level == 1.0 {
            page.width = width;
            page.height = height;
            continue;
        }
        #[allow(clippy::float_cmp)]
        if level != 5.0 {
            continue;
        }
        let text = cols[11].trim();
        let conf = n(10)?;
        if text.is_empty() || conf < 0.0 {
            continue;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (block, par, line) = (n(2)? as u32, n(3)? as u32, n(4)? as u32);
        if current != Some((block, par)) {
            page.blocks.push(Block::default());
            current = Some((block, par));
            current_line = None;
        }
        let Some(b) = page.blocks.last_mut() else {
            continue;
        };
        if current_line != Some(line) {
            b.lines.push(Line::default());
            current_line = Some(line);
        }
        if let Some(l) = b.lines.last_mut() {
            l.words.push(Word {
                text: text.to_owned(),
                conf: (conf / 100.0).clamp(0.0, 1.0),
                x0: left,
                y0: top,
                x1: left + width,
                y1: top + height,
            });
        }
    }
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bbox_layout() {
        let xhtml = r#"<doc><page width="612.000000" height="792.000000"><flow><block xMin="72" yMin="72" xMax="300" yMax="100">
<line xMin="72" yMin="72" xMax="300" yMax="84"><word xMin="72" yMin="72" xMax="100" yMax="84">The</word><word xMin="104" yMin="72" xMax="140" yMax="84">mind&amp;</word></line>
<line xMin="72" yMin="86" xMax="300" yMax="98"><word xMin="72" yMin="86" xMax="100" yMax="98">is</word></line>
</block></flow></page></doc>"#;
        let page = from_bbox_layout(xhtml, 3).unwrap();
        assert_eq!(page.number, 3);
        assert_eq!(page.width, 612.0);
        assert_eq!(page.blocks.len(), 1);
        assert_eq!(page.blocks[0].lines.len(), 2);
        assert_eq!(page.blocks[0].lines[0].text(), "The mind&");
        assert_eq!(page.chars(), 10);
    }

    #[test]
    fn reads_tesseract_tsv() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext
1\t1\t0\t0\t0\t0\t0\t0\t2550\t3300\t-1\t
2\t1\t1\t0\t0\t0\t300\t300\t1000\t100\t-1\t
3\t1\t1\t1\t0\t0\t300\t300\t1000\t100\t-1\t
4\t1\t1\t1\t1\t0\t300\t300\t1000\t40\t-1\t
5\t1\t1\t1\t1\t1\t300\t300\t100\t40\t96.5\tSitting
5\t1\t1\t1\t1\t2\t420\t300\t100\t40\t91\tquietly
4\t1\t1\t1\t2\t0\t300\t350\t1000\t40\t-1\t
5\t1\t1\t1\t2\t1\t300\t350\t100\t40\t42\tdoing
5\t1\t1\t2\t1\t1\t300\t450\t100\t40\t88\tnothing
";
        let page = from_tesseract_tsv(tsv, 7).unwrap();
        assert_eq!((page.width, page.height), (2550.0, 3300.0));
        assert_eq!(page.blocks.len(), 2);
        assert_eq!(page.blocks[0].lines.len(), 2);
        assert_eq!(page.blocks[0].lines[0].text(), "Sitting quietly");
        assert!((page.blocks[0].lines[1].words[0].conf - 0.42).abs() < 1e-6);
        assert_eq!(page.origin, Origin::Ocr);
    }
}
