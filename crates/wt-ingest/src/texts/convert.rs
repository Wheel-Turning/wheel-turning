//! Conversion for URL ingestion (Spec 08, Ingesting from URLs): the format chosen by the file's
//! signature and not its name, the main content of an HTML page, DOCX paragraphs and headings,
//! and the metadata the manifest draft starts from.

use std::io::Read as _;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;
use tl::{Node, NodeHandle, Parser};

use super::TextError;
use super::extract::{Section, decode_entities, normalize};
use super::manifest::Format;

/// What a fetched body is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Kind {
    /// A format the importer reads.
    Readable(Format),
    /// Not read, with why.
    Unsupported(String),
}

/// Chooses the format from the bytes, falling back to the content type only for text.
pub fn sniff(bytes: &[u8], content_type: Option<&str>) -> Kind {
    let ct = content_type.unwrap_or("").to_ascii_lowercase();
    if bytes.starts_with(b"%PDF") {
        return Kind::Readable(Format::Pdf);
    }
    if bytes.starts_with(b"PK\x03\x04") {
        let Ok(mut zip) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
            return Kind::Unsupported("a damaged zip archive".into());
        };
        let mut mimetype = String::new();
        if let Ok(mut f) = zip.by_name("mimetype") {
            let _ = f.read_to_string(&mut mimetype);
        }
        if mimetype.trim() == "application/epub+zip" {
            return Kind::Readable(Format::Epub);
        }
        if zip.by_name("word/document.xml").is_ok() {
            return Kind::Readable(Format::Docx);
        }
        if mimetype.contains("opendocument") {
            return Kind::Unsupported(
                "an OpenDocument file: convert it to DOCX with LibreOffice first".into(),
            );
        }
        return Kind::Unsupported("a zip archive that is neither EPUB nor DOCX".into());
    }
    if bytes.starts_with(b"{\\rtf") {
        return Kind::Unsupported("RTF: convert it to DOCX with LibreOffice first".into());
    }
    if bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]) {
        return Kind::Unsupported(
            "a Word 97 DOC: convert it to DOCX with LibreOffice first".into(),
        );
    }
    let head: String = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]).to_lowercase();
    if head.contains("<html") || head.contains("<!doctype html") || ct.starts_with("text/html") {
        return Kind::Readable(Format::Html);
    }
    if std::str::from_utf8(bytes).is_ok() && !bytes.contains(&0) {
        if ct.starts_with("text/markdown") {
            return Kind::Readable(Format::Md);
        }
        if ct.is_empty() || ct.starts_with("text/") {
            return Kind::Readable(Format::Txt);
        }
    }
    Kind::Unsupported(format!(
        "content type {:?} is not a document the importer reads",
        content_type.unwrap_or("unknown")
    ))
}

fn tag_name(node: &Node<'_>) -> Option<String> {
    node.as_tag()
        .map(|t| t.name().as_utf8_str().to_ascii_lowercase())
}

fn text_of(handle: NodeHandle, parser: &Parser<'_>) -> usize {
    handle
        .get(parser)
        .map_or(0, |n| n.inner_text(parser).split_whitespace().count())
}

/// The page's main content: its `<article>`, else `<main>` or `role="main"`, else the element
/// whose own paragraphs hold the most words; the whole page if none stands out.
pub fn html_main(html: &str) -> String {
    let Ok(dom) = tl::parse(html, tl::ParserOptions::default()) else {
        return html.to_owned();
    };
    let parser = dom.parser();
    for selector in ["article", "main", "[role=main]"] {
        if let Some(mut found) = dom.query_selector(selector)
            && let Some(handle) = found.next()
            && let Some(node) = handle.get(parser)
            && text_of(handle, parser) >= 20
        {
            return node.outer_html(parser).into_owned();
        }
    }
    let mut best: Option<(usize, NodeHandle)> = None;
    for (i, node) in dom.nodes().iter().enumerate() {
        if !matches!(tag_name(node).as_deref(), Some("div" | "section" | "td")) {
            continue;
        }
        let Some(tag) = node.as_tag() else { continue };
        let words: usize = tag
            .children()
            .top()
            .iter()
            .filter(|c| c.get(parser).and_then(tag_name).as_deref() == Some("p"))
            .map(|c| text_of(*c, parser))
            .sum();
        if words > best.map_or(0, |(w, _)| w) {
            best = Some((words, NodeHandle::new(u32::try_from(i).unwrap_or(u32::MAX))));
        }
    }
    match best {
        Some((words, handle)) if words >= 20 => handle
            .get(parser)
            .map_or_else(|| html.to_owned(), |n| n.outer_html(parser).into_owned()),
        _ => html.to_owned(),
    }
}

/// What an HTML page says about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Meta {
    /// From `og:title`, `citation_title` or `<title>`.
    pub title: Option<String>,
    /// From `author` or `citation_author`.
    pub author: Option<String>,
    /// A publication date.
    pub date: Option<String>,
    /// License signs: `rel="license"` links and Creative Commons links. Hints approve nothing.
    pub license_hints: Vec<String>,
}

/// The page's title, author, date and license hints.
pub fn html_meta(html: &str) -> Meta {
    let mut meta = Meta::default();
    let Ok(dom) = tl::parse(html, tl::ParserOptions::default()) else {
        return meta;
    };
    let parser = dom.parser();
    let mut og_title = None;
    let mut title = None;
    for node in dom.nodes() {
        let Some(tag) = node.as_tag() else { continue };
        let name = tag.name().as_utf8_str().to_ascii_lowercase();
        let attr = |k: &str| {
            tag.attributes()
                .get(k)
                .flatten()
                .map(|v| decode_entities(&v.as_utf8_str()).trim().to_owned())
        };
        match name.as_str() {
            "meta" => {
                let key = attr("property")
                    .or_else(|| attr("name"))
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let Some(content) = attr("content").filter(|c| !c.is_empty()) else {
                    continue;
                };
                match key.as_str() {
                    "og:title" | "citation_title" => og_title = og_title.or(Some(content)),
                    "author" | "citation_author" | "dc.creator" => {
                        meta.author = meta.author.or(Some(content));
                    }
                    "article:published_time" | "citation_publication_date" | "date" | "dc.date" => {
                        meta.date = meta.date.or(Some(content))
                    }
                    _ => {}
                }
            }
            "title" => {
                let t = normalize(&decode_entities(&node.inner_text(parser)));
                if !t.is_empty() {
                    title = title.or(Some(t));
                }
            }
            "a" | "link" => {
                let href = attr("href").unwrap_or_default();
                let rel = attr("rel").unwrap_or_default().to_ascii_lowercase();
                if (rel.split_whitespace().any(|r| r == "license")
                    || href.contains("creativecommons.org/"))
                    && !meta.license_hints.contains(&href)
                {
                    meta.license_hints.push(href);
                }
            }
            _ => {}
        }
    }
    meta.title = og_title.or(title);
    meta
}

static PARAGRAPH: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?s)<w:p[ >].*?</w:p>|<w:p/>").ok());
static STYLE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"<w:pStyle w:val="([^"]+)""#).ok());
static RUN_TEXT: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?s)<w:t(?:\s[^>]*)?>(.*?)</w:t>|<w:tab/>|<w:br/>").ok());

fn zip_entry(zip: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Option<String> {
    let mut f = zip.by_name(name).ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
}

/// DOCX: `word/document.xml`, a section per heading style and a block per paragraph. Footnotes
/// live in their own part, which is not read, so they are left out.
pub fn from_docx(path: &Path) -> Result<Vec<Section>, TextError> {
    let file = std::fs::File::open(path)
        .map_err(|e| TextError::Source(format!("{}: {e}", path.display())))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| TextError::Source(format!("{} is not a docx: {e}", path.display())))?;
    let xml = zip_entry(&mut zip, "word/document.xml")
        .ok_or_else(|| TextError::Source(format!("{} has no word/document.xml", path.display())))?;
    let (Some(para), Some(style), Some(run)) =
        (PARAGRAPH.as_ref(), STYLE.as_ref(), RUN_TEXT.as_ref())
    else {
        return Err(TextError::Source("DOCX patterns failed to compile".into()));
    };
    let mut sections: Vec<Section> = Vec::new();
    for p in para.find_iter(&xml) {
        let p = p.as_str();
        let heading = style
            .captures(p)
            .is_some_and(|c| c[1].to_ascii_lowercase().starts_with("heading") || &c[1] == "Title");
        let mut text = String::new();
        for c in run.captures_iter(p) {
            match c.get(1) {
                Some(t) => text.push_str(&decode_entities(t.as_str())),
                None if c[0].starts_with("<w:br") => text.push('\n'),
                None => text.push(' '),
            }
        }
        let text = normalize(&text);
        if text.is_empty() {
            continue;
        }
        if heading {
            sections.push(Section {
                title: Some(text),
                blocks: Vec::new(),
            });
        } else {
            match sections.last_mut() {
                Some(s) => s.blocks.push(text),
                None => sections.push(Section {
                    title: None,
                    blocks: vec![text],
                }),
            }
        }
    }
    sections.retain(|s| s.title.is_some() || !s.blocks.is_empty());
    Ok(sections)
}

/// A DOCX's own title and author, from `docProps/core.xml`.
pub fn docx_meta(path: &Path) -> Meta {
    let mut meta = Meta::default();
    let Some(core) = std::fs::File::open(path)
        .ok()
        .and_then(|f| zip::ZipArchive::new(f).ok())
        .and_then(|mut z| zip_entry(&mut z, "docProps/core.xml"))
    else {
        return meta;
    };
    let field = |tag: &str| {
        Regex::new(&format!(r"(?s)<{tag}[^>]*>(.*?)</{tag}>"))
            .ok()
            .and_then(|re| {
                re.captures(&core)
                    .map(|c| normalize(&decode_entities(&c[1])))
            })
            .filter(|s| !s.is_empty())
    };
    meta.title = field("dc:title");
    meta.author = field("dc:creator");
    meta.date = field("dcterms:created");
    meta
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_by_signature_not_name() {
        assert_eq!(
            sniff(b"%PDF-1.7 ...", Some("text/html")),
            Kind::Readable(Format::Pdf)
        );
        assert_eq!(
            sniff(b"<!DOCTYPE html><html><body>x</body></html>", None),
            Kind::Readable(Format::Html)
        );
        assert_eq!(
            sniff(b"plain words", Some("text/plain")),
            Kind::Readable(Format::Txt)
        );
        assert!(matches!(sniff(b"{\\rtf1 x}", None), Kind::Unsupported(_)));
        assert!(matches!(
            sniff(&[0, 1, 2, 3], Some("application/octet-stream")),
            Kind::Unsupported(_)
        ));
    }

    const PAGE: &str = r#"<html><head><title>Fixture Page</title>
<meta property="og:title" content="The Fixture Discourse">
<meta name="author" content="Fixture Author">
<meta name="citation_publication_date" content="1901">
<link rel="license" href="https://creativecommons.org/publicdomain/zero/1.0/">
</head><body>
<nav><ul><li>Home</li><li>About</li><li>Contact</li></ul></nav>
<div class="sidebar"><p>Short note.</p></div>
<div class="content"><h1>The Fixture Discourse</h1>
<p>The first teaching says the mind is like a clear pond on a still morning, and that is all.</p>
<p>The second teaching says the wind moves the water and the moon breaks into many pieces.</p>
</div>
<footer><p>Footer text with site links and a copyright line for the site itself.</p></footer>
</body></html>"#;

    #[test]
    fn keeps_the_main_content_and_drops_navigation_and_footer() {
        let main = html_main(PAGE);
        assert!(main.contains("clear pond"));
        assert!(
            !main.contains("Home") && !main.contains("Footer text") && !main.contains("Short note")
        );
        let meta = html_meta(PAGE);
        assert_eq!(meta.title.as_deref(), Some("The Fixture Discourse"));
        assert_eq!(meta.author.as_deref(), Some("Fixture Author"));
        assert_eq!(meta.date.as_deref(), Some("1901"));
        assert_eq!(meta.license_hints.len(), 1);
    }
}
