//! `wt text import` (Spec 08, Import pipeline): a corpus text from a manifest and its source
//! files into persons, a work, an edition, its units with embeddings and its locators. A dry
//! run reports and changes nothing; `--apply` writes in one transaction, and an unchanged
//! source and manifest make it a no-op.

pub mod convert;
pub mod crawl;
pub mod extract;
pub mod intake;
pub mod large;
pub mod manifest;
pub mod notice;
pub mod pdf;
pub mod proofread;
pub mod reference;
pub mod refresh;
pub mod takedown;
pub mod units;
pub mod url;
pub mod web;

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
    /// An active exclusion covers it (Spec 08, Rights lifecycle).
    #[error("excluded: {0}")]
    Excluded(String),
    /// A PDF tool failed.
    #[error("PDF tools: {0}")]
    Tool(String),
    /// A proofread could not be made or recorded.
    #[error("proofread: {0}")]
    Proof(String),
    /// The job queue refused.
    #[error(transparent)]
    Job(#[from] wt_db::job::JobError),
    /// A fetch failed or was refused.
    #[error(transparent)]
    Fetch(#[from] crate::source::IngestError),
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
    /// Tenths of the OCR pages still under `range_min_confidence` after a second reading,
    /// which the proofread spot-checks.
    pub low_ranges: Vec<pdf::LowRange>,
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
    /// For a large PDF not yet read: the chunk jobs queued.
    pub queued: Option<large::Queued>,
    /// For a manifest with parts: each part's report.
    pub parts: Vec<ImportReport>,
    /// Legal pages and notice blocks kept apart from the units.
    pub legal_blocks: usize,
    /// Copyright notices naming a holder the evidence does not cover.
    pub notices: Vec<notice::Notice>,
    /// What the notices did: `blocked: third_party_notice`, `flagged` or `held`.
    pub notice_outcome: Option<String>,
    /// Why nothing was imported, such as a text that is not English.
    pub skipped: Option<String>,
    /// Every unit's locator and content hash, for `wt text refresh`'s diff.
    #[serde(skip)]
    pub digest: Vec<(String, String)>,
}

/// Where an edition's source files are kept.
fn source_prefix(work: &str, edition: &str) -> String {
    format!("texts-source/{}/{}", slug(work), slug(edition))
}

/// The work file listing an edition's source files and their hashes, which a takedown reads.
const FILES: &str = "files.json";

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
    low_ranges: Vec<pdf::LowRange>,
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

/// Warnings for the manifest's work title and `[work] aliases` as names of the whole work
/// (Spec 08, Work-level citations): on `work_alias_stoplist`, or shorter than three characters.
pub fn work_name_warnings(
    manifest_path: &Path,
    stoplist: &[String],
) -> Result<Vec<String>, TextError> {
    let text = std::fs::read_to_string(manifest_path)
        .map_err(|e| TextError::Manifest(format!("reading {}: {e}", manifest_path.display())))?;
    let m = CorpusManifest::parse(&text)?;
    Ok(std::iter::once(&m.work.title)
        .chain(&m.work.aliases)
        .filter_map(|n| wt_extract::resolve::work_name_warning(n, stoplist))
        .collect())
}

/// How an import runs.
pub struct ImportOptions<'a> {
    /// `[licenses] allowed_license_codes`.
    pub allowed_codes: &'a [String],
    /// Who is importing.
    pub actor: &'a str,
    /// Write; without it, only report.
    pub apply: bool,
    /// Import again even when the files and manifest are unchanged: `wt text reocr`, whose new
    /// pages live in the cache.
    pub force: bool,
    /// For PDFs: the tools, scratch and the word list.
    pub pdf_env: Option<&'a PdfEnv<'a>>,
}

/// What `wt text reembed` did.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ReembedReport {
    /// Units given a new vector.
    pub reembedded: usize,
}

/// Units per transaction in [`reembed`].
const REEMBED_CHUNK: i64 = 100;

/// `wt text reembed`: gives every text unit whose vector came from another model or an older
/// `wt_ml::VECTOR_VERSION` a new vector from its stored text, without importing the edition
/// again (Spec 04; decision of October 9, 2026). Text, hashes and proofread state are left as
/// they are. Each chunk commits on its own, so an interrupted run resumes where it stopped,
/// and a rerun with nothing stale is a no-op.
pub async fn reembed(
    conn: &mut sqlx::PgConnection,
    embedder: &dyn Embedder,
) -> Result<ReembedReport, TextError> {
    let mut report = ReembedReport::default();
    loop {
        let stale = texts::stale_units(
            conn,
            embedder.model_id(),
            wt_ml::VECTOR_VERSION,
            REEMBED_CHUNK,
        )
        .await?;
        if stale.is_empty() {
            return Ok(report);
        }
        let vectors = embedder
            .embed(&stale.iter().map(|(_, t)| t.clone()).collect::<Vec<_>>())
            .await?;
        if vectors.len() != stale.len() {
            return Err(TextError::Ml(wt_ml::MlError::Output(format!(
                "{} vectors for {} units",
                vectors.len(),
                stale.len()
            ))));
        }
        let mut tx = sqlx::Acquire::begin(&mut *conn).await?;
        for ((unit, _), v) in stale.iter().zip(&vectors) {
            texts::set_unit_vector(
                &mut tx,
                *unit,
                &vector_literal(v),
                embedder.model_id(),
                wt_ml::VECTOR_VERSION,
            )
            .await?;
        }
        tx.commit().await?;
        report.reembedded += stale.len();
    }
}

/// Runs an import. `allowed_codes` is `[licenses] allowed_license_codes`. Without `apply` it
/// only reports. A PDF needs `pdf_env`.
#[allow(clippy::too_many_arguments)]
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
    import_with(
        conn,
        objects,
        embedder,
        manifest_path,
        &ImportOptions {
            allowed_codes,
            actor,
            apply,
            force: false,
            pdf_env,
        },
    )
    .await
}

/// Checks that evidence exists, is approved and carries an allowed code.
async fn check_evidence(
    conn: &mut sqlx::PgConnection,
    id: &str,
    allowed_codes: &[String],
) -> Result<(sqlx::types::Uuid, texts::ImportEvidence), TextError> {
    let evidence_id: sqlx::types::Uuid = id
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
    Ok((evidence_id, ev))
}

impl ImportReport {
    /// A report with no units yet.
    fn empty(work: &str, edition: &str) -> Self {
        Self {
            work: work.to_owned(),
            edition: edition.to_owned(),
            sections_kept: Vec::new(),
            sections_excluded: Vec::new(),
            units: 0,
            first: Vec::new(),
            last: Vec::new(),
            words: (0, 0, 0),
            warnings: Vec::new(),
            applied: false,
            unchanged: false,
            pdf: None,
            queued: None,
            parts: Vec::new(),
            legal_blocks: 0,
            notices: Vec::new(),
            notice_outcome: None,
            skipped: None,
            digest: Vec::new(),
        }
    }
}

/// One part of a compilation as a manifest of its own: its work, edition, evidence, locators,
/// and only its pages (Spec 08, Large documents, step 5).
fn part_manifest(
    m: &CorpusManifest,
    p: &manifest::PartSection,
) -> Result<CorpusManifest, TextError> {
    let (first, last) = pdf::skipped(std::slice::from_ref(&p.pages))?
        .first()
        .copied()
        .ok_or_else(|| TextError::Manifest(format!("part {:?} names no pages", p.title)))?;
    let mut d = m.clone();
    d.parts.clear();
    d.work.title.clone_from(&p.title);
    if p.author.is_some() {
        d.work.author.clone_from(&p.author);
    }
    if let Some(label) = &p.label {
        d.edition.label.clone_from(label);
    }
    if let Some(evidence) = &p.license_evidence_id {
        d.edition.license_evidence_id.clone_from(evidence);
    }
    d.units.locator_prefix.clone_from(&p.locator_prefix);
    let mut settings = d.pdf.take().unwrap_or_default();
    if first > 1 {
        settings.skip_pages.push(format!("1-{}", first - 1));
    }
    settings
        .skip_pages
        .push(format!("{}-{}", last.saturating_add(1), u32::MAX / 2));
    d.pdf = Some(settings);
    let prefix = format!("{}:", p.locator_prefix);
    d.aliases.retain(|k, _| k.starts_with(&prefix));
    Ok(d)
}

/// Runs an import with options: a large PDF not yet read is queued as chunk jobs, a manifest
/// with parts imports each part as its own edition, and anything else imports at once.
pub async fn import_with(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    embedder: &dyn Embedder,
    manifest_path: &Path,
    opts: &ImportOptions<'_>,
) -> Result<ImportReport, TextError> {
    let manifest_text = std::fs::read_to_string(manifest_path)
        .map_err(|e| TextError::Manifest(format!("reading {}: {e}", manifest_path.display())))?;
    let m = CorpusManifest::parse(&manifest_text)?;
    let dir = manifest_path.parent().unwrap_or(Path::new("."));
    let manifest_hash = sha256_hex(manifest_text.as_bytes());

    // A large PDF is read by chunk jobs first (Spec 08, Large documents, step 2).
    if m.source.format == Format::Pdf {
        let env = opts.pdf_env.ok_or_else(|| {
            TextError::Source("a PDF import needs poppler and Tesseract (PdfEnv)".into())
        })?;
        // Refused before any page is read, so a long OCR run does not end in this error.
        pdf::quality::Dictionary::check(&env.word_list)?;
        let plan = large::plan(env.tools, &m, dir)?;
        if plan.large(&m) && !plan.done(objects, &m)? {
            check_evidence(conn, &m.edition.license_evidence_id, opts.allowed_codes).await?;
            let mut hashes: Vec<String> = plan.chunks.iter().map(|c| c.sha256.clone()).collect();
            hashes.dedup();
            takedown::check_exclusions(
                conn,
                &takedown::Incoming {
                    work: &m.work.title,
                    edition: &m.edition.label,
                    source_url: m.edition.source_url.as_deref(),
                    file_hashes: &hashes,
                    unit_hashes: &[],
                },
            )
            .await?;
            for p in &m.parts {
                if let Some(e) = &p.license_evidence_id {
                    check_evidence(conn, e, opts.allowed_codes).await?;
                }
            }
            let mut report = ImportReport::empty(&m.work.title, &m.edition.label);
            if opts.apply {
                report.queued = Some(
                    large::queue(conn, objects, &m, &manifest_text, dir, &plan, opts.actor).await?,
                );
            } else {
                report.warnings.push(format!(
                    "{} pages in {} chunks are not read yet: --estimate measures them, and \
                     --apply queues ocr_chunk jobs for `wt worker --kinds ocr_chunk`",
                    plan.pages,
                    plan.chunks.len()
                ));
            }
            return Ok(report);
        }
    }

    if m.parts.is_empty() {
        let input = EditionInput {
            manifest: &m,
            manifest_text: &manifest_text,
            key_seed: &manifest_hash,
            manifest_path,
        };
        return import_one(conn, objects, embedder, &input, opts).await;
    }
    let mut report = ImportReport::empty(&m.work.title, &m.edition.label);
    report.unchanged = true;
    for (i, p) in m.parts.iter().enumerate() {
        let part = part_manifest(&m, p)?;
        let seed = sha256_hex(format!("{manifest_hash}:part:{i}").as_bytes());
        let input = EditionInput {
            manifest: &part,
            manifest_text: &manifest_text,
            key_seed: &seed,
            manifest_path,
        };
        let r = import_one(conn, objects, embedder, &input, opts).await?;
        report.units += r.units;
        report.applied |= r.applied;
        report.unchanged &= r.unchanged;
        report.parts.push(r);
    }
    Ok(report)
}

/// One edition to import: its manifest (a part's own, for a compilation), the manifest file's
/// text and path, and the seed its content key starts from.
struct EditionInput<'a> {
    /// The edition's manifest.
    manifest: &'a CorpusManifest,
    /// The manifest file as read, stored with the edition so it can be imported again.
    manifest_text: &'a str,
    /// The manifest's hash, or for a part the hash of the manifest and the part's index, so two
    /// parts of one file never share a key.
    key_seed: &'a str,
    /// Where the manifest is; source files are read relative to it.
    manifest_path: &'a Path,
}

/// The source files read into sections (Spec 08, Import pipeline, steps 2 and 3).
struct Extracted {
    /// The content key: the seed and every file's hash. An edition stored under the same key is
    /// unchanged.
    key: String,
    /// Every section of every file, before the `include_sections` filter.
    sections: Vec<extract::Section>,
    /// Each file's name and bytes, kept in the object store after a real run.
    files: Vec<(String, Vec<u8>)>,
    /// For a PDF: its words, exclusions and page facts.
    pdf: PdfPages,
}

/// Imports one edition: the dry run's checks and report, and with `--apply` the write.
async fn import_one(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    embedder: &dyn Embedder,
    input: &EditionInput<'_>,
    opts: &ImportOptions<'_>,
) -> Result<ImportReport, TextError> {
    let m = input.manifest;
    let dir = input.manifest_path.parent().unwrap_or(Path::new("."));
    let is_pdf = m.source.format == Format::Pdf;
    let settings = m.pdf.clone().unwrap_or_default();

    // 1. The evidence must exist, be approved, and carry an allowed code.
    let (evidence_id, evidence) =
        check_evidence(conn, &m.edition.license_evidence_id, opts.allowed_codes).await?;

    // 2 and 3. Extract and filter.
    let extracted = extract_sources(objects, m, dir, input.key_seed, opts.pdf_env)?;
    let (mut kept, excluded): (Vec<_>, Vec<_>) = extracted
        .sections
        .into_iter()
        .partition(|s| extract::keep(s.title.as_deref(), &m.source.include_sections));
    let (key, files, pdf_pages) = (extracted.key, extracted.files, extracted.pdf);

    // Legal pages are kept apart, whole, and never embedded or shown (Spec 08, Source copies
    // and legal pages).
    let (legal, legal_places) = notice::take_legal(&mut kept);

    if let Some(reason) = not_english(m, &kept) {
        let mut report = ImportReport::empty(&m.work.title, &m.edition.label);
        report.skipped = Some(reason);
        return Ok(report);
    }
    let (third, action) = third_party_notices(conn, m, &evidence, &legal, &kept).await?;

    // 4 and 5. Units and locators.
    let units = units::cut(&kept, &m.units, &m.work.title)?;
    if units.is_empty() {
        return Err(TextError::Empty(format!(
            "{} sections kept, none yields a unit",
            kept.len()
        )));
    }
    check_not_taken_down(conn, m, &files, &units).await?;

    let mut report = dry_run_report(
        m,
        &kept,
        &excluded,
        &units,
        legal.len(),
        &third,
        action.as_deref(),
    );
    if action.as_deref() == Some("block") {
        return Ok(report);
    }
    let notice_text: Option<String> = (!third.is_empty()).then(|| {
        third
            .iter()
            .map(|n| n.line.clone())
            .collect::<Vec<_>>()
            .join("\n")
    });
    let legal_text: Option<String> = (!legal.is_empty()).then(|| legal.join("\n\n"));

    // PDFs: each unit's page and confidence, and the suspect check (steps 4 and 6).
    let placements = if is_pdf {
        pdf::rebuild::place(units.iter().map(|u| u.text.as_str()), &pdf_pages.words)
    } else {
        Vec::new()
    };
    let confidences: Vec<Option<f32>> = (0..units.len())
        .map(|i| {
            placements
                .get(i)
                .and_then(Option::as_ref)
                .filter(|p| pdf_pages.ocr.contains(&(p.file, p.page)))
                .map(|p| p.conf)
        })
        .collect();
    if is_pdf && let Some(env) = opts.pdf_env {
        report.pdf =
            Some(pdf_report(conn, env, &units, &confidences, &settings, &pdf_pages).await?);
    }
    if report.pdf.as_ref().is_some_and(|p| p.stopped.is_some()) {
        return Ok(report);
    }
    // A dry run changes nothing in the database (Spec 08 acceptance), so it is not logged.
    if !opts.apply {
        return Ok(report);
    }
    if !opts.force
        && texts::edition_hash(conn, &m.work.title, &m.edition.label).await?
            == Some(Some(key.clone()))
    {
        report.unchanged = true;
        return Ok(report);
    }

    let ocr_engine = match opts.pdf_env {
        Some(env) if pdf_pages.ocr_pages > 0 => Some(env.tools.engine()?),
        _ => None,
    };
    let written = Written {
        input,
        key: &key,
        evidence: evidence_id,
        units: &units,
        placements: &placements,
        confidences: &confidences,
        legal_text: legal_text.as_deref(),
        legal_places: &legal_places,
        notice_text: notice_text.as_deref(),
        extraction: report
            .pdf
            .as_ref()
            .map_or("native", |p| p.extraction.as_str()),
        ocr_engine: ocr_engine.as_deref(),
        mean_confidence: report.pdf.as_ref().and_then(|p| p.mean_confidence),
        sections: (report.sections_kept.len(), report.sections_excluded.len()),
    };
    let edition = write_edition(conn, embedder, &written, opts.actor).await?;
    // A held document waits in quarantine to be released one at a time.
    if action.as_deref() == Some("hold") {
        wt_db::text_takedown::set_state_logged(
            conn,
            edition,
            "quarantined",
            &format!(
                "held for a notice: {}",
                notice_text.as_deref().unwrap_or("")
            ),
            opts.actor,
        )
        .await?;
    }
    store_sources(objects, input, &files, &units, &placements, pdf_pages)?;
    report.applied = true;
    Ok(report)
}

/// Reads every source file into sections, through the page cache and OCR for a PDF, and works
/// out the content key from `key_seed` and the files' hashes.
fn extract_sources(
    objects: &dyn ObjectStore,
    m: &CorpusManifest,
    dir: &Path,
    key_seed: &str,
    pdf_env: Option<&PdfEnv<'_>>,
) -> Result<Extracted, TextError> {
    let settings = m.pdf.clone().unwrap_or_default();
    let mut key_input = key_seed.to_owned();
    let mut sections = Vec::new();
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut pdf_pages = PdfPages::default();
    for (index, name) in m.source.files.iter().enumerate() {
        let path = dir.join(name);
        let bytes = std::fs::read(&path)
            .map_err(|e| TextError::Source(format!("{}: {e}", path.display())))?;
        let file_sha256 = sha256_hex(&bytes);
        key_input.push_str(&file_sha256);
        files.push((name.clone(), bytes));
        if m.source.format != Format::Pdf {
            sections.extend(extract::read(&path, m.source.format)?);
            continue;
        }
        let env = pdf_env.ok_or_else(|| {
            TextError::Source("a PDF import needs poppler and Tesseract (PdfEnv)".into())
        })?;
        // Pages come from the cache when an earlier run or chunk job read them.
        let cache = pdf::PageCache::new(objects, &file_sha256, &settings);
        let verse = m.units.scheme == Scheme::Verse;
        sections.extend(pdf_pages.read_file(env, &cache, &path, index, &settings, verse)?);
    }
    Ok(Extracted {
        key: sha256_hex(key_input.as_bytes()),
        sections,
        files,
        pdf: pdf_pages,
    })
}

impl PdfPages {
    /// Reads one PDF, the `index`th of the manifest's files, adds its words and page facts to
    /// these, and returns its sections. A tenth of the pages that reads weak is read once more
    /// at `reocr_dpi`, and if still weak, named in `low_ranges` for the proofread (Spec 08,
    /// Large documents, step 6).
    fn read_file(
        &mut self,
        env: &PdfEnv<'_>,
        cache: &pdf::PageCache<'_>,
        path: &Path,
        index: usize,
        settings: &pdf::PdfSettings,
        verse: bool,
    ) -> Result<Vec<extract::Section>, TextError> {
        let cached = pdf::ReadOptions {
            cache: Some(cache),
            ..pdf::ReadOptions::default()
        };
        let mut read = pdf::read(env.tools, path, settings, &env.scratch, cached)?;
        let mut reread = false;
        for (first, last, _) in pdf::weak_ranges(&read, settings.range_min_confidence) {
            let low_dpi = read
                .pages
                .iter()
                .zip(&read.dpis)
                .any(|(p, d)| (first..=last).contains(&p.number) && *d < settings.reocr_dpi);
            if low_dpi {
                pdf::read(
                    env.tools,
                    path,
                    settings,
                    &env.scratch,
                    pdf::ReadOptions {
                        cache: Some(cache),
                        pages: Some((first, last)),
                        force_dpi: Some(settings.reocr_dpi),
                    },
                )?;
                reread = true;
            }
        }
        if reread {
            read = pdf::read(env.tools, path, settings, &env.scratch, cached)?;
        }
        self.low_ranges.extend(
            pdf::weak_ranges(&read, settings.range_min_confidence)
                .into_iter()
                .map(|(first, last, mean_confidence)| pdf::LowRange {
                    file: index,
                    first,
                    last,
                    mean_confidence,
                }),
        );
        let mut rebuilt = pdf::rebuild::rebuild(&read.pages, settings.strip_running_heads, verse);
        for w in &mut rebuilt.words {
            w.file = index;
        }
        for (page, dpi) in read.pages.iter().zip(&read.dpis) {
            self.sizes
                .insert((index, page.number), pdf::page_points(page, *dpi));
            if page.origin == pdf::layout::Origin::Ocr {
                self.ocr.insert((index, page.number));
                self.ocr_confidences.extend(page.words().map(|w| w.conf));
            }
        }
        self.text_layer_pages += read.text_layer_pages;
        self.ocr_pages += read.ocr_pages;
        self.words.extend(rebuilt.words);
        self.excluded.extend(rebuilt.excluded);
        Ok(rebuilt.sections)
    }
}

/// Why the edition is skipped when it is not English, which is all v1 loads (Spec 08, Mandated
/// sources): the manifest says another language, or too few of the first 200 blocks' words are
/// common English ones.
fn not_english(m: &CorpusManifest, kept: &[extract::Section]) -> Option<String> {
    let sample: String = kept
        .iter()
        .flat_map(|s| s.blocks.iter())
        .take(200)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    if m.edition.language.eq_ignore_ascii_case("en") && notice::is_english(&sample) {
        return None;
    }
    Some(format!(
        "not English (language {:?}, {:.0}% common English words): only English is loaded in v1",
        m.edition.language,
        notice::english_share(&sample) * 100.0
    ))
}

/// The notice scan: the legal pages and the first and last blocks. Returns the notices naming
/// a holder that neither the manifest's rights holder nor the evidence covers, and what to do
/// about them: nothing when there are none, else the mandated source's `notice_action` (`flag`
/// or `hold`), or `block`.
async fn third_party_notices(
    conn: &mut sqlx::PgConnection,
    m: &CorpusManifest,
    evidence: &texts::ImportEvidence,
    legal: &[String],
    kept: &[extract::Section],
) -> Result<(Vec<notice::Notice>, Option<String>), TextError> {
    let found = notice::notices(
        legal
            .iter()
            .map(String::as_str)
            .chain(notice::edges(kept, 3)),
    );
    let mut covers: Vec<&str> = vec![m.edition.rights_holder.as_str()];
    covers.extend(evidence.publisher.as_deref());
    covers.extend(evidence.grantor.as_deref());
    let third = notice::third_party(&found, &covers);
    if third.is_empty() {
        return Ok((third, None));
    }
    let source_action = match &m.edition.source_key {
        Some(key) => wt_db::text_takedown::notice_action(conn, key).await?,
        None => None,
    };
    Ok((
        third,
        Some(source_action.unwrap_or_else(|| "block".to_owned())),
    ))
}

/// Refuses what was taken down before: not the work, not the source, not the same content at
/// another address (Spec 08, Rights lifecycle, "Never use it again"). Also refuses a
/// `source_key` that names no source.
async fn check_not_taken_down(
    conn: &mut sqlx::PgConnection,
    m: &CorpusManifest,
    files: &[(String, Vec<u8>)],
    units: &[Unit],
) -> Result<(), TextError> {
    let file_hashes: Vec<String> = files.iter().map(|(_, b)| sha256_hex(b)).collect();
    let unit_hashes: Vec<String> = units
        .iter()
        .map(|u| sha256_hex(u.text.as_bytes()))
        .collect();
    takedown::check_exclusions(
        conn,
        &takedown::Incoming {
            work: &m.work.title,
            edition: &m.edition.label,
            source_url: m.edition.source_url.as_deref(),
            file_hashes: &file_hashes,
            unit_hashes: &unit_hashes,
        },
    )
    .await?;
    if let Some(key) = &m.edition.source_key
        && wt_db::text_takedown::source_enabled(conn, key)
            .await?
            .is_none()
    {
        return Err(TextError::Manifest(format!(
            "source_key {key:?} is not a source"
        )));
    }
    Ok(())
}

/// The report a dry run prints: sections kept and left out, the first and last units, word
/// counts, warnings for long units and unused aliases, and what the notices did. `units` is
/// never empty.
fn dry_run_report(
    m: &CorpusManifest,
    kept: &[extract::Section],
    excluded: &[extract::Section],
    units: &[Unit],
    legal_blocks: usize,
    third: &[notice::Notice],
    action: Option<&str>,
) -> ImportReport {
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
    ImportReport {
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
        queued: None,
        parts: Vec::new(),
        legal_blocks,
        notices: third.to_vec(),
        notice_outcome: action.map(|a| match a {
            "flag" => "flagged".to_owned(),
            "hold" => "held".to_owned(),
            _ => "blocked: third_party_notice".to_owned(),
        }),
        skipped: None,
        digest: units
            .iter()
            .map(|u| (u.locator.clone(), sha256_hex(u.text.as_bytes())))
            .collect(),
    }
}

/// What a PDF import found, with the suspect check (Spec 08, PDFs and OCR): a unit is suspect
/// when its OCR confidence or its share of dictionary words (the English word list and the
/// glossary) is low, and the import stops when more than `max_suspect_share` of the units are.
async fn pdf_report(
    conn: &mut sqlx::PgConnection,
    env: &PdfEnv<'_>,
    units: &[Unit],
    confidences: &[Option<f32>],
    settings: &pdf::PdfSettings,
    pages: &PdfPages,
) -> Result<PdfReport, TextError> {
    let glossary: Vec<String> = wt_db::glossary::load(conn)
        .await?
        .into_iter()
        .flat_map(|t| std::iter::once(t.canonical).chain(t.variants))
        .collect();
    let dictionary = pdf::quality::Dictionary::load(&env.word_list, &glossary)?;
    let suspect: Vec<pdf::quality::Suspect> = units
        .iter()
        .zip(confidences)
        .filter_map(|(u, &confidence)| {
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
    Ok(PdfReport {
        text_layer_pages: pages.text_layer_pages,
        ocr_pages: pages.ocr_pages,
        extraction: if pages.ocr_pages > 0 {
            "ocr"
        } else {
            "text_layer"
        }
        .into(),
        mean_confidence: pdf::layout::mean(pages.ocr_confidences.iter().copied()),
        excluded_blocks: pages.excluded.clone(),
        suspect,
        suspect_share,
        stopped,
        low_ranges: pages.low_ranges.clone(),
    })
}

/// What a real run writes for one edition, all of it worked out and checked by the dry run.
struct Written<'a> {
    /// The edition's manifest and where it came from.
    input: &'a EditionInput<'a>,
    /// The content key the edition is stored under.
    key: &'a str,
    /// The approved license evidence.
    evidence: sqlx::types::Uuid,
    /// The units, in order.
    units: &'a [Unit],
    /// For a PDF, where each unit starts; empty otherwise.
    placements: &'a [Option<pdf::rebuild::Placement>],
    /// Each unit's OCR confidence, when it starts on an OCR page.
    confidences: &'a [Option<f32>],
    /// The legal pages, kept apart and never embedded.
    legal_text: Option<&'a str>,
    /// Where the legal pages were.
    legal_places: &'a [String],
    /// The third-party notices' lines, for a flagged or held document.
    notice_text: Option<&'a str>,
    /// `native`, `text_layer` or `ocr`.
    extraction: &'a str,
    /// The OCR engine and version, when any page was read by OCR.
    ocr_engine: Option<&'a str>,
    /// The mean OCR word confidence.
    mean_confidence: Option<f32>,
    /// Sections kept and left out, for the import run's counts.
    sections: (usize, usize),
}

/// Steps 6 and 7: embeds the units, then writes the persons, work, edition, units, locators and
/// the import run in one transaction. Returns the edition.
async fn write_edition(
    conn: &mut sqlx::PgConnection,
    embedder: &dyn Embedder,
    w: &Written<'_>,
    actor: &str,
) -> Result<sqlx::types::Uuid, TextError> {
    let m = w.input.manifest;
    let vectors = embedder
        .embed(&w.units.iter().map(|u| u.text.clone()).collect::<Vec<_>>())
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
    if !m.work.aliases.is_empty() {
        wt_db::locator::set_work_aliases(&mut tx, work, &m.work.aliases).await?;
    }
    let edition = texts::upsert_edition(
        &mut tx,
        &NewEdition {
            work,
            label: &m.edition.label,
            translator,
            language: &m.edition.language,
            source_url: m.edition.source_url.as_deref(),
            rights_holder: &m.edition.rights_holder,
            evidence: w.evidence,
            excerptable: true,
            content_hash: w.key,
            source_key: m.edition.source_key.as_deref(),
            legal_text: w.legal_text,
            legal_pages: serde_json::json!(w.legal_places),
            notice: w.notice_text,
            extraction: w.extraction,
            ocr_engine: w.ocr_engine,
            mean_confidence: w.mean_confidence,
        },
    )
    .await?;
    let new_units: Vec<NewUnit<'_>> = w
        .units
        .iter()
        .zip(&vectors)
        .enumerate()
        .map(|(i, (u, v))| {
            let placed = w.placements.get(i).and_then(Option::as_ref);
            NewUnit {
                ordinal: i32::try_from(u.ordinal).unwrap_or(i32::MAX),
                locator: &u.locator,
                label: &u.label,
                heading: u.heading.as_deref(),
                text: &u.text,
                word_count: i32::try_from(u.word_count).unwrap_or(i32::MAX),
                content_hash: sha256_hex(u.text.as_bytes()),
                embedding: vector_literal(v),
                source_page: placed.and_then(|p| i32::try_from(p.page).ok()),
                confidence: w.confidences.get(i).copied().flatten(),
                source_anchor: source_anchor(m.source.format, &u.text, placed.map(|p| p.page)),
            }
        })
        .collect();
    texts::replace_units(
        &mut tx,
        edition,
        &new_units,
        embedder.model_id(),
        wt_ml::VECTOR_VERSION,
    )
    .await?;

    // 7. Locators, with any aliases from the manifest.
    let locators: Vec<(String, String, Vec<String>)> = w
        .units
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
        "units": w.units.len(),
        "sections_kept": w.sections.0,
        "sections_excluded": w.sections.1,
        "key": w.key,
    });
    texts::record_run(
        &mut tx,
        &w.input.manifest_path.display().to_string(),
        "corpus",
        false,
        &counts,
        actor,
    )
    .await?;
    tx.commit().await?;
    Ok(edition)
}

/// Keeps what a later run needs after the edition is written: the source files and their
/// hashes, so the import can be repeated and a takedown can find them (Spec 08, step 6), the
/// manifest for `wt text reocr` and `wt text refresh`, and for a PDF where each unit stands,
/// for the proof sheet's crops. Geometry only, never page images.
fn store_sources(
    objects: &dyn ObjectStore,
    input: &EditionInput<'_>,
    files: &[(String, Vec<u8>)],
    units: &[Unit],
    placements: &[Option<pdf::rebuild::Placement>],
    pdf_pages: PdfPages,
) -> Result<(), TextError> {
    let m = input.manifest;
    let prefix = source_prefix(&m.work.title, &m.edition.label);
    for (name, bytes) in files {
        objects.put(&format!("{prefix}/{name}"), bytes)?;
    }
    let listed: Vec<takedown::StoredFile> = files
        .iter()
        .map(|(name, bytes)| takedown::StoredFile {
            name: name.clone(),
            sha256: sha256_hex(bytes),
        })
        .collect();
    let listed =
        serde_json::to_vec(&listed).map_err(|e| TextError::Source(format!("files.json: {e}")))?;
    let work = work_prefix(&m.work.title, &m.edition.label);
    objects.put(&format!("{work}/{FILES}"), &listed)?;
    objects.put(
        &format!("{work}/manifest.toml"),
        input.manifest_text.as_bytes(),
    )?;
    if m.source.format != Format::Pdf {
        return Ok(());
    }
    let mut stored = pdf::Placements {
        files: m.source.files.clone(),
        units: BTreeMap::new(),
        low_ranges: pdf_pages.low_ranges,
    };
    for (u, p) in units.iter().zip(placements) {
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
    let json =
        serde_json::to_vec(&stored).map_err(|e| TextError::Source(format!("placements: {e}")))?;
    objects.put(&format!("{work}/{}", pdf::PLACEMENTS), &json)?;
    Ok(())
}

/// Where a unit starts in its original (Spec 08, Ingesting from URLs, source links): a text
/// fragment of its opening words for HTML, so a browser highlights the excerpt, and the page for
/// a PDF. The page joins it to the edition's source URL.
fn source_anchor(format: Format, text: &str, page: Option<u32>) -> Option<String> {
    match format {
        Format::Html => {
            let opening: Vec<&str> = text.split_whitespace().take(6).collect();
            (!opening.is_empty()).then(|| {
                format!(
                    "#:~:text={}",
                    percent_encoding::utf8_percent_encode(
                        &opening.join(" "),
                        percent_encoding::NON_ALPHANUMERIC
                    )
                )
            })
        }
        Format::Pdf => page.map(|p| format!("#page={p}")),
        _ => None,
    }
}
