//! PDFs and OCR (Spec 08): each page read from its text layer when it has a usable one and
//! through Tesseract when it does not, the text rebuilt from the page layouts, and each unit
//! traced back to its page and confidence. Page images are working files in scratch, deleted
//! as soon as they are read, and never stored.
//!
//! The vision pass is not built: model text could otherwise reach a displayed quotation, which
//! rule 5 forbids (decision of October 7, 2026). Suspect pages go to the proofread instead.

pub mod layout;
pub mod proof;
pub mod quality;
pub mod rebuild;
pub mod tools;

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::TextError;
use layout::{Origin, Page};
use tools::PdfTools;

/// The object, beside an edition's work files, that records where each unit stands on its page,
/// for the proof sheet's crops.
pub const PLACEMENTS: &str = "placements.json";

/// When to OCR a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OcrMode {
    /// A page without a usable text layer.
    #[default]
    Auto,
    /// Every page.
    Always,
    /// No page.
    Never,
}

/// `[pdf]` in a corpus manifest.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdfSettings {
    /// When to OCR.
    #[serde(default)]
    pub ocr: OcrMode,
    /// Tesseract languages.
    #[serde(default = "eng")]
    pub languages: Vec<String>,
    /// Render resolution for OCR.
    #[serde(default = "dpi")]
    pub dpi: u32,
    /// `auto`, `1` or `2`; Tesseract's automatic page segmentation finds the columns.
    #[serde(default = "auto")]
    pub columns: String,
    /// Pages to leave out, such as `1-6` or `9`.
    #[serde(default)]
    pub skip_pages: Vec<String>,
    /// Drop lines repeated in the margin of several pages.
    #[serde(default = "yes")]
    pub strip_running_heads: bool,
    /// `off`; `flagged` is refused while the vision pass is deferred.
    #[serde(default = "off")]
    pub vision_pass: String,
    /// A page with fewer characters in its text layer goes to OCR.
    #[serde(default = "min_layer_chars")]
    pub min_layer_chars: usize,
    /// A unit with a lower mean word confidence is suspect.
    #[serde(default = "min_unit_confidence")]
    pub min_unit_confidence: f32,
    /// A unit with a lower share of known words is suspect.
    #[serde(default = "min_dictionary_share")]
    pub min_dictionary_share: f32,
    /// More suspect units than this share stop the import.
    #[serde(default = "max_suspect_share")]
    pub max_suspect_share: f32,
}

fn eng() -> Vec<String> {
    vec!["eng".into()]
}
fn dpi() -> u32 {
    300
}
fn auto() -> String {
    "auto".into()
}
fn yes() -> bool {
    true
}
fn off() -> String {
    "off".into()
}
fn min_layer_chars() -> usize {
    200
}
fn min_unit_confidence() -> f32 {
    0.80
}
fn min_dictionary_share() -> f32 {
    0.85
}
fn max_suspect_share() -> f32 {
    0.10
}

impl Default for PdfSettings {
    fn default() -> Self {
        Self {
            ocr: OcrMode::Auto,
            languages: eng(),
            dpi: dpi(),
            columns: auto(),
            skip_pages: Vec::new(),
            strip_running_heads: true,
            vision_pass: off(),
            min_layer_chars: min_layer_chars(),
            min_unit_confidence: min_unit_confidence(),
            min_dictionary_share: min_dictionary_share(),
            max_suspect_share: max_suspect_share(),
        }
    }
}

impl PdfSettings {
    /// Checks the values.
    pub fn check(&self) -> Result<(), TextError> {
        if self.vision_pass != "off" {
            return Err(TextError::Manifest(format!(
                "vision_pass = {:?}: the vision pass is deferred, so model text never becomes a \
                 quotation (decision of October 7, 2026); use \"off\" and proofread",
                self.vision_pass
            )));
        }
        if !matches!(self.columns.as_str(), "auto" | "1" | "2") {
            return Err(TextError::Manifest(format!(
                "columns = {:?}; use auto, 1 or 2",
                self.columns
            )));
        }
        if !(72..=1200).contains(&self.dpi) {
            return Err(TextError::Manifest(format!(
                "dpi = {} is out of range",
                self.dpi
            )));
        }
        for share in [
            self.min_unit_confidence,
            self.min_dictionary_share,
            self.max_suspect_share,
        ] {
            if !(0.0..=1.0).contains(&share) {
                return Err(TextError::Manifest(format!(
                    "{share} is not a share from 0 to 1"
                )));
            }
        }
        skipped(&self.skip_pages).map(|_| ())
    }
}

/// The page numbers `skip_pages` names.
pub fn skipped(ranges: &[String]) -> Result<Vec<(u32, u32)>, TextError> {
    ranges
        .iter()
        .map(|r| {
            let bad = || TextError::Manifest(format!("skip_pages: {r:?} is not a page or a range"));
            let (a, b) = r.split_once('-').unwrap_or((r, r));
            let a: u32 = a.trim().parse().map_err(|_| bad())?;
            let b: u32 = b.trim().parse().map_err(|_| bad())?;
            if a == 0 || b < a {
                return Err(bad());
            }
            Ok((a, b))
        })
        .collect()
}

/// Whether a text layer can be read as it is: enough characters, mostly letters.
pub fn usable_layer(page: &Page, min_chars: usize) -> bool {
    let chars: Vec<char> = page.words().flat_map(|w| w.text.chars()).collect();
    let letters = chars.iter().filter(|c| c.is_alphabetic()).count();
    chars.len() >= min_chars && letters * 2 >= chars.len()
}

/// A PDF's pages as read.
#[derive(Debug, Clone, Default)]
pub struct Read {
    /// The pages kept, in order.
    pub pages: Vec<Page>,
    /// Pages read from the text layer.
    pub text_layer_pages: usize,
    /// Pages read by OCR.
    pub ocr_pages: usize,
}

/// Reads a PDF page by page: never the whole document at once, and each page image deleted
/// once Tesseract has read it.
pub fn read(
    tools: &dyn PdfTools,
    pdf: &Path,
    settings: &PdfSettings,
    scratch: &Path,
) -> Result<Read, TextError> {
    let skip = skipped(&settings.skip_pages)?;
    let count = tools.page_count(pdf)?;
    std::fs::create_dir_all(scratch)
        .map_err(|e| TextError::Source(format!("{}: {e}", scratch.display())))?;
    let mut out = Read::default();
    for n in 1..=count {
        if skip.iter().any(|&(a, b)| (a..=b).contains(&n)) {
            continue;
        }
        let layer = match settings.ocr {
            OcrMode::Always => None,
            _ => Some(tools.text_layer(pdf, n)?),
        };
        let page = match (settings.ocr, layer) {
            (OcrMode::Never, Some(page)) => page,
            (OcrMode::Auto, Some(page)) if usable_layer(&page, settings.min_layer_chars) => page,
            _ => {
                let png = scratch.join(format!("ocr-page-{n}.png"));
                let rendered = tools.render(pdf, n, settings.dpi, None, &png);
                let page = rendered.and_then(|()| tools.ocr(&png, &settings.languages, n));
                // Rule 6's discipline for images too: deleted on success and on failure.
                let _ = std::fs::remove_file(&png);
                page?
            }
        };
        match page.origin {
            Origin::TextLayer => out.text_layer_pages += 1,
            Origin::Ocr => out.ocr_pages += 1,
        }
        out.pages.push(page);
    }
    Ok(out)
}

/// Where one unit stands, for the proof sheet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnitPlace {
    /// Index into `Placements::files`.
    pub file: usize,
    /// Its first page.
    pub page: u32,
    /// Its box on that page, as shares of the page.
    pub frac: [f32; 4],
    /// The page's width in points.
    pub w_pts: f32,
    /// Its height in points.
    pub h_pts: f32,
}

/// Every unit's place, stored beside the edition's work files (geometry only, no images).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Placements {
    /// The source files, in manifest order.
    pub files: Vec<String>,
    /// By locator.
    pub units: BTreeMap<String, UnitPlace>,
}

/// A page's size in points: a text layer is measured in points, a scan in pixels at `dpi`.
pub fn page_points(page: &Page, dpi: u32) -> (f32, f32) {
    match page.origin {
        Origin::TextLayer => (page.width, page.height),
        Origin::Ocr => {
            #[allow(clippy::cast_precision_loss)]
            let scale = 72.0 / dpi.max(1) as f32;
            (page.width * scale, page.height * scale)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_settings_with_defaults() {
        let s: PdfSettings = toml::from_str("skip_pages = [\"1-6\", \"9\"]").unwrap();
        assert_eq!(s.dpi, 300);
        assert_eq!(s.ocr, OcrMode::Auto);
        assert_eq!(skipped(&s.skip_pages).unwrap(), vec![(1, 6), (9, 9)]);
        s.check().unwrap();
    }

    #[test]
    fn refuses_the_vision_pass_and_bad_ranges() {
        let s: PdfSettings = toml::from_str("vision_pass = \"flagged\"").unwrap();
        assert!(s.check().unwrap_err().to_string().contains("deferred"));
        let s: PdfSettings = toml::from_str("skip_pages = [\"6-1\"]").unwrap();
        assert!(s.check().is_err());
    }
}
