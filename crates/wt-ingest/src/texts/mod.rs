//! `wt text import` (Spec 08, Import pipeline): a corpus text from a manifest and its source
//! files into persons, a work, an edition, its units with embeddings and its locators. A dry
//! run reports and changes nothing; `--apply` writes in one transaction, and an unchanged
//! source and manifest make it a no-op.

pub mod extract;
pub mod manifest;
pub mod pdf;
pub mod proofread;
pub mod reference;
pub mod units;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use wt_core::objects::ObjectStore;
use wt_core::records::sha256_hex;
use wt_db::texts::{self, NewEdition, NewUnit};
use wt_ml::Embedder;

use manifest::{CorpusManifest, Format, Scheme};
use pdf::rebuild::{Excluded, Placed};
use pdf::tools::PdfTools;
use units::Unit;

/// Why an import failed or was refused.
#[derive(Debug, thiserror::Error)]
pub enum TextError {
    /// The manifest is unreadable or incomplete.
    #[error("manifest: {0}")]
    Manifest(String),
    /// A source file could not be read.
    #[error("source: {0}")]
    Source(String),
    /// The license evidence does not allow the import.
    #[error("license evidence: {0}")]
    Evidence(String),
    /// Two units would share a locator.
    #[error("duplicate locator {0}")]
    DuplicateLocator(String),
    /// Nothing to import after filtering.
    #[error("no units: {0}")]
    Empty(String),
    /// A PDF tool failed.
    #[error("PDF tools: {0}")]
    Tool(String),
    /// A proofread could not be made or recorded.
    #[error("proofread: {0}")]
    Proof(String),
    /// The embedder failed.
    #[error(transparent)]
    Ml(#[from] wt_ml::MlError),
    /// The database refused.
    #[error(transparent)]
    Db(#[from] wt_db::DbError),
    /// A connection error.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// The object store refused.
    #[error(transparent)]
    Objects(#[from] wt_core::objects::ObjectError),
}

/// A unit as the report shows it: locator and the start of its text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Preview {
    /// Its locator.
    pub locator: String,
    /// Its label.
    pub label: String,
    /// The first 80 characters.
    pub start: String,
}

impl From<&Unit> for Preview {
    fn from(u: &Unit) -> Self {
        Self {
            locator: u.locator.clone(),
            label: u.label.clone(),
            start: u.text.chars().take(80).collect(),
        }
    }
}

/// What a PDF import found (Spec 08, PDFs and OCR).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PdfReport {
    /// Pages read from their text layer.
    pub text_layer_pages: usize,
    /// Pages read by OCR.
    pub ocr_pages: usize,
    /// `text_layer` or `ocr`.
    pub extraction: String,
    /// The mean OCR word confidence.
    pub mean_confidence: Option<f32>,
    /// Every block left out: page numbers, running heads and footnotes.
    pub excluded_blocks: Vec<Excluded>,
    /// Suspect units.
    pub suspect: Vec<pdf::quality::Suspect>,
    /// Their share of all units.
    pub suspect_share: f32,
    /// Why the import stopped, when too many units are suspect.
    pub stopped: Option<String>,
}

/// What a PDF import needs besides the manifest: the tools, a scratch directory for page
/// images and the English word list.
pub struct PdfEnv<'a> {
    /// poppler and Tesseract.
    pub tools: &'a dyn PdfTools,
    /// Where page images are written and deleted.
    pub scratch: PathBuf,
    /// One word to a line.
    pub word_list: PathBuf,
}

/// What an import found and did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImportReport {
    /// The work's title.
    pub work: String,
    /// The edition's label.
    pub edition: String,
    /// Section titles kept.
    pub sections_kept: Vec<String>,
    /// Section titles excluded.
    pub sections_excluded: Vec<String>,
    /// Units cut.
    pub units: usize,
    /// The first three units.
    pub first: Vec<Preview>,
    /// The last three units.
    pub last: Vec<Preview>,
    /// Fewest, median and most words in a unit.
    pub words: (usize, usize, usize),
    /// Notes for the person reading the report.
    pub warnings: Vec<String>,
    /// Whether anything was written.
    pub applied: bool,
    /// Whether the edition was already imported from these files and this manifest.
    pub unchanged: bool,
    /// For a PDF: pages, exclusions and suspect units.
    pub pdf: Option<PdfReport>,
}

/// Where an edition's source files are kept.
fn source_prefix(work: &str, edition: &str) -> String {
    format!("texts-source/{}/{}", slug(work), slug(edition))
}

/// Where an edition's working files are kept: geometry and text, never images.
fn work_prefix(work: &str, edition: &str) -> String {
    format!("texts-work/{}/{}", slug(work), slug(edition))
}

/// The PDF side of an import: the rebuilt words, exclusions and page facts.
#[derive(Default)]
struct PdfPages {
    words: Vec<Placed>,
    excluded: Vec<Excluded>,
    sizes: BTreeMap<(usize, u32), (f32, f32)>,
    ocr: BTreeSet<(usize, u32)>,
    ocr_confidences: Vec<f32>,
    text_layer_pages: usize,
    ocr_pages: usize,
}

fn vector_literal(v: &[f32]) -> String {
    let parts: Vec<String> = v.iter().map(f32::to_string).collect();
    format!("[{}]", parts.join(","))
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

/// Runs an import. `allowed_codes` is `[licenses] allowed_license_codes`. Without `apply` it
/// only reports. A PDF needs `pdf_env`.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub async fn import(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    embedder: &dyn Embedder,
    manifest_path: &Path,
    allowed_codes: &[String],
    actor: &str,
    apply: bool,
    pdf_env: Option<&PdfEnv<'_>>,
) -> Result<ImportReport, TextError> {
    let manifest_text = std::fs::read_to_string(manifest_path)
        .map_err(|e| TextError::Manifest(format!("reading {}: {e}", manifest_path.display())))?;
    let m = CorpusManifest::parse(&manifest_text)?;
    let dir = manifest_path.parent().unwrap_or(Path::new("."));

    // 1. The evidence must exist, be approved, and carry an allowed code.
    let evidence_id: sqlx::types::Uuid = m
        .edition
        .license_evidence_id
        .parse()
        .map_err(|e| TextError::Evidence(format!("license_evidence_id: {e}")))?;
    let ev = texts::evidence(conn, evidence_id)
        .await?
        .ok_or_else(|| TextError::Evidence(format!("{evidence_id} does not exist")))?;
    if !ev.allowed {
        return Err(TextError::Evidence(format!(
            "{evidence_id} is not approved; a person approves it after reading the snapshot"
        )));
    }
    if !allowed_codes.iter().any(|c| c == &ev.license_code) {
        return Err(TextError::Evidence(format!(
            "{} is not in allowed_license_codes",
            ev.license_code
        )));
    }

    // 2 and 3. Extract and filter.
    let mut key_input = sha256_hex(manifest_text.as_bytes());
    let mut sections = Vec::new();
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let is_pdf = m.source.format == Format::Pdf;
    let settings = m.pdf.clone().unwrap_or_default();
    let mut pdf_pages = PdfPages::default();
    for (index, name) in m.source.files.iter().enumerate() {
        let path = dir.join(name);
        let bytes = std::fs::read(&path)
            .map_err(|e| TextError::Source(format!("{}: {e}", path.display())))?;
        key_input.push_str(&sha256_hex(&bytes));
        files.push((name.clone(), bytes));
        if !is_pdf {
            sections.extend(extract::read(&path, m.source.format)?);
            continue;
        }
        let env = pdf_env.ok_or_else(|| {
            TextError::Source("a PDF import needs poppler and Tesseract (PdfEnv)".into())
        })?;
        let read = pdf::read(env.tools, &path, &settings, &env.scratch)?;
        let mut rebuilt = pdf::rebuild::rebuild(
            &read.pages,
            settings.strip_running_heads,
            m.units.scheme == Scheme::Verse,
        );
        for w in &mut rebuilt.words {
            w.file = index;
        }
        for page in &read.pages {
            pdf_pages
                .sizes
                .insert((index, page.number), pdf::page_points(page, settings.dpi));
            if page.origin == pdf::layout::Origin::Ocr {
                pdf_pages.ocr.insert((index, page.number));
                pdf_pages
                    .ocr_confidences
                    .extend(page.words().map(|w| w.conf));
            }
        }
        pdf_pages.text_layer_pages += read.text_layer_pages;
        pdf_pages.ocr_pages += read.ocr_pages;
        pdf_pages.words.extend(rebuilt.words);
        pdf_pages.excluded.extend(rebuilt.excluded);
        sections.extend(rebuilt.sections);
    }
    let key = sha256_hex(key_input.as_bytes());
    let (kept, excluded): (Vec<_>, Vec<_>) = sections
        .into_iter()
        .partition(|s| extract::keep(s.title.as_deref(), &m.source.include_sections));

    // 4 and 5. Units and locators.
    let units = units::cut(&kept, &m.units, &m.work.title)?;
    if units.is_empty() {
        return Err(TextError::Empty(format!(
            "{} sections kept, none yields a unit",
            kept.len()
        )));
    }
    let mut lengths: Vec<usize> = units.iter().map(|u| u.word_count).collect();
    lengths.sort_unstable();
    let mut warnings = Vec::new();
    if let Some(l) = units.iter().find(|u| u.word_count > m.units.max_words) {
        warnings.push(format!("{} has {} words", l.locator, l.word_count));
    }
    for alias_key in m.aliases.keys() {
        if !units.iter().any(|u| &u.locator == alias_key) {
            warnings.push(format!("[aliases] names {alias_key}, which no unit has"));
        }
    }
    let title_of = |s: &extract::Section| s.title.clone().unwrap_or_else(|| "(untitled)".into());
    let mut report = ImportReport {
        work: m.work.title.clone(),
        edition: m.edition.label.clone(),
        sections_kept: kept.iter().map(title_of).collect(),
        sections_excluded: excluded.iter().map(title_of).collect(),
        units: units.len(),
        first: units.iter().take(3).map(Preview::from).collect(),
        last: units
            .iter()
            .skip(units.len().saturating_sub(3))
            .map(Preview::from)
            .collect(),
        words: (
            lengths[0],
            lengths[lengths.len() / 2],
            lengths[lengths.len() - 1],
        ),
        warnings,
        applied: false,
        unchanged: false,
        pdf: None,
    };

    // PDFs: each unit's page and confidence, and the suspect check (steps 4 and 6).
    let placements = if is_pdf {
        pdf::rebuild::place(units.iter().map(|u| u.text.as_str()), &pdf_pages.words)
    } else {
        Vec::new()
    };
    let unit_confidence = |i: usize| -> Option<f32> {
        placements
            .get(i)
            .and_then(Option::as_ref)
            .filter(|p| pdf_pages.ocr.contains(&(p.file, p.page)))
            .map(|p| p.conf)
    };
    if is_pdf && let Some(env) = pdf_env {
        let glossary: Vec<String> = wt_db::glossary::load(conn)
            .await?
            .into_iter()
            .flat_map(|t| std::iter::once(t.canonical).chain(t.variants))
            .collect();
        let dictionary = pdf::quality::Dictionary::load(&env.word_list, &glossary)?;
        let suspect: Vec<pdf::quality::Suspect> = units
            .iter()
            .enumerate()
            .filter_map(|(i, u)| {
                let confidence = unit_confidence(i);
                let share = dictionary.share(&u.text);
                pdf::quality::suspect(
                    confidence,
                    share,
                    settings.min_unit_confidence,
                    settings.min_dictionary_share,
                )
                .then(|| pdf::quality::Suspect {
                    locator: u.locator.clone(),
                    confidence,
                    dictionary_share: share,
                })
            })
            .collect();
        #[allow(clippy::cast_precision_loss)]
        let suspect_share = suspect.len() as f32 / units.len() as f32;
        let stopped = (suspect_share > settings.max_suspect_share).then(|| {
            format!(
                "{} of {} units are suspect ({:.0}%, over max_suspect_share {:.0}%): the scan \
                 probably needs better settings or a better copy",
                suspect.len(),
                units.len(),
                suspect_share * 100.0,
                settings.max_suspect_share * 100.0
            )
        });
        report.pdf = Some(PdfReport {
            text_layer_pages: pdf_pages.text_layer_pages,
            ocr_pages: pdf_pages.ocr_pages,
            extraction: if pdf_pages.ocr_pages > 0 {
                "ocr"
            } else {
                "text_layer"
            }
            .into(),
            mean_confidence: pdf::layout::mean(pdf_pages.ocr_confidences.iter().copied()),
            excluded_blocks: pdf_pages.excluded.clone(),
            suspect,
            suspect_share,
            stopped,
        });
    }
    if report.pdf.as_ref().is_some_and(|p| p.stopped.is_some()) {
        return Ok(report);
    }
    // A dry run changes nothing in the database (Spec 08 acceptance), so it is not logged.
    if !apply {
        return Ok(report);
    }
    if texts::edition_hash(conn, &m.work.title, &m.edition.label).await? == Some(Some(key.clone()))
    {
        report.unchanged = true;
        return Ok(report);
    }

    let ocr_engine = match pdf_env {
        Some(env) if pdf_pages.ocr_pages > 0 => Some(env.tools.engine()?),
        _ => None,
    };

    // 6. Embed, then one transaction.
    let vectors = embedder
        .embed(&units.iter().map(|u| u.text.clone()).collect::<Vec<_>>())
        .await?;
    let mut tx = sqlx::Acquire::begin(&mut *conn).await?;
    let author = match &m.work.author {
        Some(a) => Some(texts::upsert_person(&mut tx, a, "author").await?),
        None => None,
    };
    let translator = match &m.edition.translator {
        Some(t) => Some(texts::upsert_person(&mut tx, t, "translator").await?),
        None => None,
    };
    let work =
        texts::upsert_work(&mut tx, &m.work.title, author, m.work.tradition.as_deref()).await?;
    let edition = texts::upsert_edition(
        &mut tx,
        &NewEdition {
            work,
            label: &m.edition.label,
            translator,
            language: &m.edition.language,
            source_url: m.edition.source_url.as_deref(),
            rights_holder: &m.edition.rights_holder,
            evidence: evidence_id,
            excerptable: true,
            content_hash: &key,
            extraction: report
                .pdf
                .as_ref()
                .map_or("native", |p| p.extraction.as_str()),
            ocr_engine: ocr_engine.as_deref(),
            mean_confidence: report.pdf.as_ref().and_then(|p| p.mean_confidence),
        },
    )
    .await?;
    let new_units: Vec<NewUnit<'_>> = units
        .iter()
        .zip(&vectors)
        .enumerate()
        .map(|(i, (u, v))| NewUnit {
            ordinal: i32::try_from(u.ordinal).unwrap_or(i32::MAX),
            locator: &u.locator,
            label: &u.label,
            heading: u.heading.as_deref(),
            text: &u.text,
            word_count: i32::try_from(u.word_count).unwrap_or(i32::MAX),
            content_hash: sha256_hex(u.text.as_bytes()),
            embedding: vector_literal(v),
            source_page: placements
                .get(i)
                .and_then(Option::as_ref)
                .and_then(|p| i32::try_from(p.page).ok()),
            confidence: unit_confidence(i),
        })
        .collect();
    texts::replace_units(&mut tx, edition, &new_units, embedder.model_id()).await?;

    // 7. Locators, with any aliases from the manifest.
    let locators: Vec<(String, String, Vec<String>)> = units
        .iter()
        .map(|u| {
            (
                u.locator.clone(),
                u.label.clone(),
                m.aliases.get(&u.locator).cloned().unwrap_or_default(),
            )
        })
        .collect();
    texts::sync_locators(&mut tx, work, &locators).await?;
    let counts = serde_json::json!({
        "units": units.len(),
        "sections_kept": report.sections_kept.len(),
        "sections_excluded": report.sections_excluded.len(),
        "key": key,
    });
    texts::record_run(
        &mut tx,
        &manifest_path.display().to_string(),
        "corpus",
        false,
        &counts,
        actor,
    )
    .await?;
    tx.commit().await?;

    // The source files, kept so the import can be repeated (Spec 08, step 6).
    let prefix = source_prefix(&m.work.title, &m.edition.label);
    for (name, bytes) in &files {
        objects.put(&format!("{prefix}/{name}"), bytes)?;
    }
    // Where each unit stands, for the proof sheet's crops: geometry only.
    if is_pdf {
        let mut stored = pdf::Placements {
            files: m.source.files.clone(),
            units: BTreeMap::new(),
        };
        for (u, p) in units.iter().zip(&placements) {
            if let Some(p) = p {
                let (w_pts, h_pts) = pdf_pages
                    .sizes
                    .get(&(p.file, p.page))
                    .copied()
                    .unwrap_or((612.0, 792.0));
                stored.units.insert(
                    u.locator.clone(),
                    pdf::UnitPlace {
                        file: p.file,
                        page: p.page,
                        frac: p.frac,
                        w_pts,
                        h_pts,
                    },
                );
            }
        }
        let json = serde_json::to_vec(&stored)
            .map_err(|e| TextError::Source(format!("placements: {e}")))?;
        objects.put(
            &format!(
                "{}/{}",
                work_prefix(&m.work.title, &m.edition.label),
                pdf::PLACEMENTS
            ),
            &json,
        )?;
    }
    report.applied = true;
    Ok(report)
}
