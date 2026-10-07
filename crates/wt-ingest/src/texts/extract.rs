//! Extraction (Spec 08, Import pipeline, steps 2 and 3): sections of text blocks from EPUB,
//! HTML, plain text and Markdown, normalized, with the default front and back matter filtered.
//!
//! EPUB is read as what it is, a zip of XHTML files in the order the package's spine gives,
//! with `zip` and the same HTML reader, because the `epub` crate is GPL-3.0, which `cargo deny`
//! refuses. HTML is parsed with `tl` (MIT) rather than `scraper`, whose `cssparser` is
//! MPL-2.0, which `deny.toml` does not allow (decisions of October 7, 2026).

use std::io::Read;
use std::path::Path;

use regex::Regex;
use tl::{Node, NodeHandle, Parser};
use unicode_normalization::UnicodeNormalization;

use super::TextError;
use super::manifest::Format;

/// A titled stretch of text: blocks are paragraphs or stanzas, line breaks kept inside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Its heading, if any.
    pub title: Option<String>,
    /// Its blocks, in order.
    pub blocks: Vec<String>,
}

/// NFKC, straight quotes, single spaces within lines, trimmed lines.
pub fn normalize(text: &str) -> String {
    let folded: String = text
        .nfkc()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201F}' => '"',
            '\u{00A0}' => ' ',
            c => c,
        })
        .collect();
    folded
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

/// A line that is only a page number, Arabic or Roman.
fn page_number(line: &str) -> bool {
    let l = line.trim();
    !l.is_empty()
        && l.len() <= 6
        && (l.chars().all(|c| c.is_ascii_digit())
            || l.chars().all(|c| "ivxlcdmIVXLCDM".contains(c)) && l.len() <= 5)
}

fn tidy_block(text: &str) -> Option<String> {
    let lines: Vec<String> = normalize(text)
        .lines()
        .filter(|l| !page_number(l))
        .map(str::to_owned)
        .collect();
    let block = lines.join("\n").trim().to_owned();
    (!block.is_empty()).then_some(block)
}

/// Blocks of plain text, split on blank lines.
fn text_blocks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if let Some(b) = tidy_block(&cur) {
                out.push(b);
            }
            cur.clear();
        } else {
            cur.push_str(line);
            cur.push('\n');
        }
    }
    if let Some(b) = tidy_block(&cur) {
        out.push(b);
    }
    out
}

/// Plain text: one untitled section.
pub fn from_txt(text: &str) -> Vec<Section> {
    vec![Section {
        title: None,
        blocks: text_blocks(text),
    }]
}

/// Markdown: a section per heading, the text before the first heading untitled.
pub fn from_md(text: &str) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut title: Option<String> = None;
    let mut body = String::new();
    for line in text.lines() {
        if let Some(h) = line.trim_start().strip_prefix('#') {
            let blocks = text_blocks(&body);
            if title.is_some() || !blocks.is_empty() {
                sections.push(Section {
                    title: title.take(),
                    blocks,
                });
            }
            title = Some(normalize(h.trim_start_matches('#')));
            body.clear();
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    let blocks = text_blocks(&body);
    if title.is_some() || !blocks.is_empty() {
        sections.push(Section { title, blocks });
    }
    sections
}

/// Decodes the character references HTML text keeps: the common named ones and numeric ones.
pub fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(end) = tail.find(';').filter(|&e| e <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let name = &tail[1..end];
        let decoded = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            "mdash" => Some('\u{2014}'),
            "ndash" => Some('\u{2013}'),
            "hellip" => Some('\u{2026}'),
            "lsquo" | "rsquo" => Some('\''),
            "ldquo" | "rdquo" => Some('"'),
            _ => name
                .strip_prefix("#x")
                .or_else(|| name.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn tag_name(node: &Node<'_>) -> Option<String> {
    node.as_tag()
        .map(|t| t.name().as_utf8_str().to_ascii_lowercase())
}

fn children(node: &Node<'_>) -> Vec<NodeHandle> {
    node.as_tag()
        .map(|t| t.children().top().iter().copied().collect())
        .unwrap_or_default()
}

/// The text under a node, with `<br>` as a line break and footnote markers left out: `<sup>`
/// and links whose text is a bare number or bracketed number.
fn node_text(handle: NodeHandle, parser: &Parser<'_>, out: &mut String) {
    let Some(node) = handle.get(parser) else {
        return;
    };
    match node {
        Node::Raw(bytes) => out.push_str(&decode_entities(&bytes.as_utf8_str())),
        Node::Comment(_) => {}
        Node::Tag(_) => match tag_name(node).as_deref() {
            Some("br") => out.push('\n'),
            Some("sup" | "script" | "style") => {}
            Some("a") => {
                let mut inner = String::new();
                for c in children(node) {
                    node_text(c, parser, &mut inner);
                }
                let bare = inner.trim().trim_matches(|c| c == '[' || c == ']');
                if !(bare.chars().all(|c| c.is_ascii_digit()) && !bare.is_empty()) {
                    out.push_str(&inner);
                }
            }
            _ => {
                for c in children(node) {
                    node_text(c, parser, out);
                }
            }
        },
    }
}

const BLOCKS: [&str; 9] = ["h1", "h2", "h3", "h4", "h5", "h6", "p", "li", "pre"];

/// Walks the document in order, starting a section at each heading and taking a block from
/// each paragraph, list item and preformatted element.
fn walk(handle: NodeHandle, parser: &Parser<'_>, sections: &mut Vec<Section>) {
    let Some(node) = handle.get(parser) else {
        return;
    };
    let Some(name) = tag_name(node) else {
        return;
    };
    // Navigation, page furniture and forms are not text (Spec 08, conversion).
    if matches!(
        name.as_str(),
        "script" | "style" | "head" | "nav" | "header" | "footer" | "aside" | "form" | "noscript"
    ) {
        return;
    }
    if BLOCKS.contains(&name.as_str()) {
        let mut text = String::new();
        node_text(handle, parser, &mut text);
        if name.len() == 2 && name.starts_with('h') {
            let t = normalize(&text);
            sections.push(Section {
                title: (!t.is_empty()).then_some(t),
                blocks: Vec::new(),
            });
        } else if let Some(b) = tidy_block(&text) {
            match sections.last_mut() {
                Some(s) => s.blocks.push(b),
                None => sections.push(Section {
                    title: None,
                    blocks: vec![b],
                }),
            }
        }
        return;
    }
    for c in children(node) {
        walk(c, parser, sections);
    }
}

/// `<x .../>` as `<x ...></x>`.
fn expand_self_closing(html: &str) -> String {
    match Regex::new(r"<([A-Za-z][\w:-]*)((?:\s[^<>]*?)?)\s*/>") {
        Ok(re) => re.replace_all(html, "<$1$2></$1>").into_owned(),
        Err(_) => html.to_owned(),
    }
}

/// HTML: a section per heading, blocks from paragraphs, list items and preformatted text.
pub fn from_html(html: &str) -> Vec<Section> {
    // `tl` does not end a self-closing tag such as XHTML's `<br/>` and would nest what follows
    // inside it, so each one is written out as an empty element first.
    let html = expand_self_closing(html);
    let Ok(dom) = tl::parse(&html, tl::ParserOptions::default()) else {
        return Vec::new();
    };
    let parser = dom.parser();
    let mut sections = Vec::new();
    for handle in dom.children() {
        walk(*handle, parser, &mut sections);
    }
    sections.retain(|s| s.title.is_some() || !s.blocks.is_empty());
    sections
}

/// Every element named `tag` in a small XML document, as its attributes.
fn xml_elements(xml: &str, tag: &str) -> Vec<std::collections::HashMap<String, String>> {
    let Ok(dom) = tl::parse(xml, tl::ParserOptions::default()) else {
        return Vec::new();
    };
    dom.nodes()
        .iter()
        .filter_map(Node::as_tag)
        .filter(|t| t.name().as_utf8_str().eq_ignore_ascii_case(tag))
        .map(|t| {
            t.attributes()
                .iter()
                .map(|(k, v)| {
                    (
                        k.to_lowercase(),
                        v.map(|v| v.into_owned()).unwrap_or_default(),
                    )
                })
                .collect()
        })
        .collect()
}

fn zip_text(zip: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Result<String, TextError> {
    let mut file = zip
        .by_name(name)
        .map_err(|e| TextError::Source(format!("{name} in the epub: {e}")))?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|e| TextError::Source(format!("reading {name}: {e}")))?;
    Ok(text)
}

/// EPUB: the package's spine, in order, each document through the HTML reader.
pub fn from_epub(path: &Path) -> Result<Vec<Section>, TextError> {
    let file = std::fs::File::open(path)
        .map_err(|e| TextError::Source(format!("{}: {e}", path.display())))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| TextError::Source(format!("{} is not an epub: {e}", path.display())))?;
    let container = zip_text(&mut zip, "META-INF/container.xml")?;
    let rootfile = xml_elements(&container, "rootfile")
        .into_iter()
        .find_map(|a| a.get("full-path").cloned())
        .ok_or_else(|| TextError::Source("the epub names no package".into()))?;
    let opf = zip_text(&mut zip, &rootfile)?;
    let base = rootfile
        .rsplit_once('/')
        .map_or("", |(dir, _)| dir)
        .to_owned();
    let items: std::collections::HashMap<String, String> = xml_elements(&opf, "item")
        .into_iter()
        .filter_map(|a| Some((a.get("id")?.clone(), a.get("href")?.clone())))
        .collect();
    let spine: Vec<String> = xml_elements(&opf, "itemref")
        .into_iter()
        .filter_map(|a| a.get("idref").cloned())
        .collect();
    let mut sections = Vec::new();
    for id in spine {
        let Some(href) = items.get(&id) else {
            continue;
        };
        let name = if base.is_empty() {
            href.clone()
        } else {
            format!("{base}/{href}")
        };
        sections.extend(from_html(&zip_text(&mut zip, &name)?));
    }
    Ok(sections)
}

/// Reads one source file.
pub fn read(path: &Path, format: Format) -> Result<Vec<Section>, TextError> {
    let text = || {
        std::fs::read_to_string(path)
            .map_err(|e| TextError::Source(format!("{}: {e}", path.display())))
    };
    match format {
        Format::Txt => Ok(from_txt(&text()?)),
        Format::Md => Ok(from_md(&text()?)),
        // The page's main content, as a readability extractor picks it (Spec 08, Ingesting
        // from URLs, conversion).
        Format::Html => Ok(from_html(&super::convert::html_main(&text()?))),
        Format::Docx => super::convert::from_docx(path),
        Format::Epub => from_epub(path),
        Format::Pdf => Err(TextError::Source(
            "a PDF is read page by page through texts::pdf, with its tools".into(),
        )),
    }
}

/// Titles excluded by default (Spec 08, step 3): front and back matter, where translator
/// material often carries its own copyright.
pub fn default_excluded(title: &str) -> bool {
    static WORDS: [&str; 11] = [
        "introduction",
        "preface",
        "foreword",
        "translator",
        "notes",
        "commentary",
        "glossary",
        "index",
        "bibliography",
        "appendix",
        "about",
    ];
    let t = title.to_lowercase();
    WORDS.iter().any(|w| {
        t.split(|c: char| !c.is_alphanumeric())
            .any(|word| word == *w || (*w == "translator" && word.starts_with("translator")))
    })
}

/// `*` and `?` globs, case-insensitive.
fn glob(pattern: &str) -> Option<Regex> {
    let mut re = String::from("(?i)^");
    for c in pattern.chars() {
        match c {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    Regex::new(&re).ok()
}

/// Whether a section is kept: an include glob restores it, otherwise the default exclusions
/// apply. Untitled sections are kept.
pub fn keep(title: Option<&str>, include: &[String]) -> bool {
    let Some(t) = title else {
        return true;
    };
    if include
        .iter()
        .filter_map(|g| glob(g))
        .any(|re| re.is_match(t))
    {
        return true;
    }
    !default_excluded(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_keeps_line_breaks_and_drops_footnote_markers() {
        let s = from_html(
            "<html><body><h2>Chapter 1: The Pairs</h2>\
             <p>All that we are<br/>is the result of what we have thought.<sup>1</sup></p>\
             <p>See the note <a href=\"#n2\">[2]</a> here.</p><p>17</p></body></html>",
        );
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].title.as_deref(), Some("Chapter 1: The Pairs"));
        assert_eq!(
            s[0].blocks,
            vec![
                "All that we are\nis the result of what we have thought.".to_owned(),
                "See the note here.".to_owned()
            ],
            "the bare page number goes too"
        );
    }

    #[test]
    fn character_references_are_decoded() {
        assert_eq!(
            decode_entities("A &amp; B &#8212; &#x41;&nbsp;&bogus; &"),
            "A & B \u{2014} A &bogus; &"
        );
    }

    #[test]
    fn normalizing_straightens_quotes_and_keeps_diacritics() {
        assert_eq!(
            normalize("\u{201C}Dōgen\u{2019}s  way\u{201D}"),
            "\"Dōgen's way\""
        );
    }

    #[test]
    fn front_matter_is_excluded_unless_included() {
        assert!(!keep(Some("Translator's Preface"), &[]));
        assert!(!keep(Some("Notes"), &[]));
        assert!(keep(Some("Chapter 1"), &[]));
        assert!(keep(None, &[]));
        assert!(keep(Some("Notes"), &["note*".into()]));
        assert!(!keep(Some("Introduction"), &["Chapter*".into()]));
    }

    #[test]
    fn markdown_sections_follow_headings() {
        let s = from_md("Untitled lead.\n\n# One\n\nFirst para.\n\nSecond\npara.\n## Two\nThird.");
        let titles: Vec<_> = s.iter().map(|x| x.title.clone()).collect();
        assert_eq!(titles, vec![None, Some("One".into()), Some("Two".into())]);
        assert_eq!(s[1].blocks, vec!["First para.", "Second\npara."]);
    }
}
