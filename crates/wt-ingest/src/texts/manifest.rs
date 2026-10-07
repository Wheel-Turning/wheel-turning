//! The import manifest (Spec 08, Import pipeline): a small TOML file beside the source files.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use super::TextError;

/// A corpus text's manifest.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusManifest {
    /// Always `corpus`.
    pub role: String,
    /// The work.
    pub work: WorkSection,
    /// The edition.
    pub edition: EditionSection,
    /// The files.
    pub source: SourceSection,
    /// How units are cut.
    pub units: UnitsSection,
    /// Extra citation aliases per locator, for the resolver.
    #[serde(default)]
    pub aliases: BTreeMap<String, Vec<String>>,
    /// For a PDF: how its pages are read (Spec 08, PDFs and OCR).
    #[serde(default)]
    pub pdf: Option<super::pdf::PdfSettings>,
    /// For a PDF that binds several texts: each becomes its own work or edition, with its own
    /// pages, locators, evidence and proofread (Spec 08, Large documents, step 5).
    #[serde(default)]
    pub parts: Vec<PartSection>,
}

/// `[[parts]]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartSection {
    /// The part's work title.
    pub title: String,
    /// Its edition label; the manifest's by default.
    #[serde(default)]
    pub label: Option<String>,
    /// Its pages, such as `1-120`.
    pub pages: String,
    /// Its locators' prefix.
    pub locator_prefix: String,
    /// Its author, when not the manifest's.
    #[serde(default)]
    pub author: Option<String>,
    /// Its evidence, when not the manifest's.
    #[serde(default)]
    pub license_evidence_id: Option<String>,
}

/// `[work]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkSection {
    /// The work's title.
    pub title: String,
    /// A person's display name; none for an anonymous work.
    #[serde(default)]
    pub author: Option<String>,
    /// Such as `theravada`.
    #[serde(default)]
    pub tradition: Option<String>,
}

/// `[edition]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditionSection {
    /// Such as `Muller 1881`.
    pub label: String,
    /// A person's display name.
    #[serde(default)]
    pub translator: Option<String>,
    /// Language code.
    #[serde(default = "english")]
    pub language: String,
    /// Where the source came from.
    #[serde(default)]
    pub source_url: Option<String>,
    /// Who holds the rights, or "public domain".
    pub rights_holder: String,
    /// The approved evidence.
    pub license_evidence_id: String,
    /// The source it comes through, such as a mandated publisher, so disabling the source
    /// stops it serving.
    #[serde(default)]
    pub source_key: Option<String>,
}

fn english() -> String {
    "en".into()
}

/// The source formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// An EPUB file, read in spine order.
    Epub,
    /// An HTML page.
    Html,
    /// Plain text.
    Txt,
    /// Markdown.
    Md,
    /// A PDF.
    Pdf,
    /// A Word document.
    Docx,
}

/// `[source]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSection {
    /// File names beside the manifest.
    pub files: Vec<String>,
    /// Their format.
    pub format: Format,
    /// Globs on section titles that restore sections the default exclusions would drop, such as
    /// a commentary the evidence covers.
    #[serde(default)]
    pub include_sections: Vec<String>,
}

/// How units are cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
    /// Blank-line stanzas, line breaks kept.
    Verse,
    /// Split where a line starts with the number pattern.
    Numbered,
    /// Blank-line paragraphs.
    Paragraph,
    /// Each heading's body.
    Heading,
}

/// `[units]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnitsSection {
    /// The scheme.
    pub scheme: Scheme,
    /// Such as `dhammapada`.
    pub locator_prefix: String,
    /// For `numbered`: a regular expression whose first group is the number.
    #[serde(default)]
    pub number_pattern: Option<String>,
    /// Such as `{work}, verse {n}`; may use `{work}`, `{n}`, `{chapter}` and `{heading}`.
    pub label_template: String,
    /// Longer units split at sentence boundaries into `.a`, `.b` and so on.
    #[serde(default = "max_words")]
    pub max_words: usize,
    /// Number units within each section, as `<prefix>:<chapter>:<n>`.
    #[serde(default)]
    pub chapters: bool,
}

fn max_words() -> usize {
    220
}

impl CorpusManifest {
    /// Reads and checks a manifest.
    pub fn load(path: &Path) -> Result<Self, TextError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| TextError::Manifest(format!("reading {}: {e}", path.display())))?;
        Self::parse(&text)
    }

    /// Parses and checks a manifest's text.
    pub fn parse(text: &str) -> Result<Self, TextError> {
        let m: Self = toml::from_str(text).map_err(|e| TextError::Manifest(e.to_string()))?;
        if m.role != "corpus" {
            return Err(TextError::Manifest(format!(
                "role is {:?}; `wt text import` reads corpus manifests",
                m.role
            )));
        }
        if m.edition.rights_holder.trim().is_empty() {
            return Err(TextError::Manifest("rights_holder is blank".into()));
        }
        if m.source.files.is_empty() {
            return Err(TextError::Manifest("[source] names no files".into()));
        }
        if m.units.scheme == Scheme::Numbered && m.units.number_pattern.is_none() {
            return Err(TextError::Manifest(
                "the numbered scheme needs number_pattern".into(),
            ));
        }
        match (&m.pdf, m.source.format) {
            (Some(pdf), Format::Pdf) => pdf.check()?,
            (Some(_), _) => {
                return Err(TextError::Manifest(
                    "[pdf] is for a PDF source; set format = \"pdf\" or remove it".into(),
                ));
            }
            (None, _) => {}
        }
        if !m.parts.is_empty() {
            if m.source.format != Format::Pdf {
                return Err(TextError::Manifest("[[parts]] split a PDF by pages".into()));
            }
            let mut titles = std::collections::BTreeSet::new();
            let mut prefixes = std::collections::BTreeSet::new();
            for p in &m.parts {
                super::pdf::skipped(std::slice::from_ref(&p.pages))?;
                if !titles.insert(p.title.as_str()) || !prefixes.insert(p.locator_prefix.as_str()) {
                    return Err(TextError::Manifest(format!(
                        "part {:?}: titles and locator prefixes must differ between parts",
                        p.title
                    )));
                }
            }
        }
        if text.contains("REPLACE_ME") {
            return Err(TextError::Manifest(
                "the manifest still holds REPLACE_ME placeholders".into(),
            ));
        }
        Ok(m)
    }
}
