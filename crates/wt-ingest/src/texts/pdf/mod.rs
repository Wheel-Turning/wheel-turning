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
use wt_core::objects::ObjectStore;
use wt_core::records::sha256_hex;

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
    /// A PDF with more pages than this is read by `ocr_chunk` jobs of this many pages.
    #[serde(default = "chunk_pages")]
    pub chunk_pages: u32,
    /// A tenth of the OCR pages with a lower mean confidence is read again at `reocr_dpi`, and
    /// if still lower, spot-checked in the proofread.
    #[serde(default = "range_min_confidence")]
    pub range_min_confidence: f32,
    /// The resolution of that second reading.
    #[serde(default = "reocr_dpi")]
    pub reocr_dpi: u32,
    /// The cap on vision-pass pages; accepted, and unused while the vision pass is deferred.
    #[serde(default = "vision_max_pages")]
    pub vision_max_pages: u32,
}

/// `languages`' default: English, the only language v1 loads.
fn eng() -> Vec<String> {
    vec!["eng".into()]
}
/// `dpi`'s default for rendering a page to OCR.
fn dpi() -> u32 {
    300
}
/// `columns`' default: Tesseract's automatic page segmentation finds the columns.
fn auto() -> String {
    "auto".into()
}
/// `strip_running_heads`' default: on.
fn yes() -> bool {
    true
}
/// `vision_pass`' default: off, since the vision pass is deferred.
fn off() -> String {
    "off".into()
}
/// `min_layer_chars`' default: a page whose text layer has fewer characters is read by OCR.
fn min_layer_chars() -> usize {
    200
}
/// `min_unit_confidence`' default: a unit under this mean OCR confidence is suspect.
fn min_unit_confidence() -> f32 {
    0.80
}
/// `min_dictionary_share`' default: a unit with fewer dictionary words than this is suspect.
fn min_dictionary_share() -> f32 {
    0.85
}
/// `max_suspect_share`' default: the import stops when more units than this are suspect.
fn max_suspect_share() -> f32 {
    0.10
}
/// `chunk_pages`' default: pages per chunk job for a large PDF.
fn chunk_pages() -> u32 {
    50
}
/// `range_min_confidence`' default: a tenth of the pages under this mean confidence is read again.
fn range_min_confidence() -> f32 {
    0.85
}
/// `reocr_dpi`' default for that second reading.
fn reocr_dpi() -> u32 {
    400
}
/// `vision_max_pages`' default, accepted and unused while the vision pass is deferred.
fn vision_max_pages() -> u32 {
    200
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
            chunk_pages: chunk_pages(),
            range_min_confidence: range_min_confidence(),
            reocr_dpi: reocr_dpi(),
            vision_max_pages: vision_max_pages(),
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
        if self.chunk_pages == 0 {
            return Err(TextError::Manifest("chunk_pages must be at least 1".into()));
        }
        for share in [
            self.range_min_confidence,
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
    /// The resolution each OCR page was read at, beside `pages`; 0 for a text layer.
    pub dpis: Vec<u32>,
    /// Pages read from the text layer.
    pub text_layer_pages: usize,
    /// Pages read by OCR.
    pub ocr_pages: usize,
    /// Pages read now rather than found in the cache.
    pub read_now: usize,
}

/// A page as the cache keeps it: its layout and the resolution it was read at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedPage {
    /// 0 for a text layer.
    pub dpi: u32,
    /// The layout: words, confidences and geometry, never an image.
    pub page: Page,
}

/// Pages already read, kept in the object store by the PDF's hash and the settings that change
/// how a page is read (Spec 08, Large documents, step 2). A rerun reads only the pages it lacks,
/// and every part, chunk and edition of the same file shares them.
pub struct PageCache<'a> {
    objects: &'a dyn ObjectStore,
    prefix: String,
}

impl<'a> PageCache<'a> {
    /// The cache for one PDF read with `settings`.
    pub fn new(objects: &'a dyn ObjectStore, pdf_sha256: &str, settings: &PdfSettings) -> Self {
        let read_settings = format!(
            "{:?}|{}|{}|{}",
            settings.ocr,
            settings.dpi,
            settings.languages.join("+"),
            settings.min_layer_chars
        );
        Self {
            objects,
            prefix: format!(
                "texts-work/_pages/{}/{}",
                pdf_sha256,
                &sha256_hex(read_settings.as_bytes())[..16]
            ),
        }
    }

    /// The object key for one cached page.
    fn key(&self, page: u32) -> String {
        format!("{}/{page}.json", self.prefix)
    }

    /// A cached page.
    pub fn get(&self, page: u32) -> Result<Option<CachedPage>, TextError> {
        self.objects
            .get(&self.key(page))?
            .map(|b| {
                serde_json::from_slice(&b)
                    .map_err(|e| TextError::Source(format!("cached page {page}: {e}")))
            })
            .transpose()
    }

    /// Keeps a page.
    pub fn put(&self, page: u32, cached: &CachedPage) -> Result<(), TextError> {
        let bytes = serde_json::to_vec(cached)
            .map_err(|e| TextError::Source(format!("caching page {page}: {e}")))?;
        Ok(self.objects.put(&self.key(page), &bytes)?)
    }

    /// Marks a chunk of pages done.
    pub fn mark_chunk(&self, first: u32, last: u32) -> Result<(), TextError> {
        Ok(self
            .objects
            .put(&format!("{}/chunks/{first}-{last}.done", self.prefix), b"")?)
    }

    /// Whether a chunk of pages is done.
    pub fn chunk_done(&self, first: u32, last: u32) -> Result<bool, TextError> {
        Ok(self
            .objects
            .get(&format!("{}/chunks/{first}-{last}.done", self.prefix))?
            .is_some())
    }
}

/// How much of a PDF to read, and how.
#[derive(Default, Clone, Copy)]
pub struct ReadOptions<'a> {
    /// Pages already read, and where to keep new ones.
    pub cache: Option<&'a PageCache<'a>>,
    /// Only these pages, first to last.
    pub pages: Option<(u32, u32)>,
    /// OCR every page in range at this resolution, cached or not: `wt text reocr` and the
    /// per-range check.
    pub force_dpi: Option<u32>,
}

/// OCRs one page: rendered into scratch, read, and the image deleted on success and failure.
fn ocr_page(
    tools: &dyn PdfTools,
    pdf: &Path,
    n: u32,
    dpi: u32,
    languages: &[String],
    scratch: &Path,
) -> Result<Page, TextError> {
    let png = scratch.join(format!("ocr-page-{n}.png"));
    let rendered = tools.render(pdf, n, dpi, None, &png);
    let page = rendered.and_then(|()| tools.ocr(&png, languages, n));
    let _ = std::fs::remove_file(&png);
    page
}

/// Reads a PDF page by page: never the whole document at once, each page image deleted once
/// Tesseract has read it, and each page found in or added to the cache.
pub fn read(
    tools: &dyn PdfTools,
    pdf: &Path,
    settings: &PdfSettings,
    scratch: &Path,
    options: ReadOptions<'_>,
) -> Result<Read, TextError> {
    let skip = skipped(&settings.skip_pages)?;
    let count = tools.page_count(pdf)?;
    std::fs::create_dir_all(scratch)
        .map_err(|e| TextError::Source(format!("{}: {e}", scratch.display())))?;
    let (first, last) = options.pages.unwrap_or((1, count));
    let mut out = Read::default();
    for n in first.max(1)..=last.min(count) {
        if skip.iter().any(|&(a, b)| (a..=b).contains(&n)) {
            continue;
        }
        let cached = match (options.cache, options.force_dpi) {
            (Some(cache), None) => cache.get(n)?,
            _ => None,
        };
        let cached = match cached {
            Some(c) => c,
            None => {
                out.read_now += 1;
                let fresh = if let Some(dpi) = options.force_dpi {
                    CachedPage {
                        dpi,
                        page: ocr_page(tools, pdf, n, dpi, &settings.languages, scratch)?,
                    }
                } else {
                    let layer = match settings.ocr {
                        OcrMode::Always => None,
                        _ => Some(tools.text_layer(pdf, n)?),
                    };
                    match (settings.ocr, layer) {
                        (OcrMode::Never, Some(page)) => CachedPage { dpi: 0, page },
                        (OcrMode::Auto, Some(page))
                            if usable_layer(&page, settings.min_layer_chars) =>
                        {
                            CachedPage { dpi: 0, page }
                        }
                        _ => CachedPage {
                            dpi: settings.dpi,
                            page: ocr_page(
                                tools,
                                pdf,
                                n,
                                settings.dpi,
                                &settings.languages,
                                scratch,
                            )?,
                        },
                    }
                };
                if let Some(cache) = options.cache {
                    cache.put(n, &fresh)?;
                }
                fresh
            }
        };
        match cached.page.origin {
            Origin::TextLayer => out.text_layer_pages += 1,
            Origin::Ocr => out.ocr_pages += 1,
        }
        out.dpis.push(cached.dpi);
        out.pages.push(cached.page);
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
    /// Weak page ranges the proofread spot-checks.
    #[serde(default)]
    pub low_ranges: Vec<LowRange>,
}

/// A tenth of a file's OCR pages whose mean confidence stayed under `range_min_confidence`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LowRange {
    /// Index into the manifest's files.
    pub file: usize,
    /// Its first page.
    pub first: u32,
    /// Its last page.
    pub last: u32,
    /// Its mean word confidence.
    pub mean_confidence: f32,
}

/// The tenths of the OCR pages read whose mean word confidence is under `min`, as first page,
/// last page and mean.
pub fn weak_ranges(read: &Read, min: f32) -> Vec<(u32, u32, f32)> {
    let ocr: Vec<&Page> = read
        .pages
        .iter()
        .filter(|p| p.origin == Origin::Ocr)
        .collect();
    let n = ocr.len();
    let mut out = Vec::new();
    for t in 0..10 {
        let group = &ocr[t * n / 10..(t + 1) * n / 10];
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        if let Some(mean) = layout::mean(group.iter().flat_map(|p| p.words().map(|w| w.conf)))
            && mean < min
        {
            out.push((first.number, last.number, mean));
        }
    }
    out
}

/// A page's size in points: a text layer is measured in points, a scan in pixels at `dpi`.
pub fn page_points(page: &Page, dpi: u32) -> (f32, f32) {
    match page.origin {
        Origin::TextLayer => (page.width, page.height),
        Origin::Ocr if dpi == 0 => (page.width, page.height),
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
