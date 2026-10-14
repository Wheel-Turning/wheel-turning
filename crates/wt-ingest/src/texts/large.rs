//! Large documents (Spec 08): a PDF over `chunk_pages` pages is read by `ocr_chunk` jobs on the
//! talks' queue, one per page range, each writing its pages' layouts to the page cache and never
//! an image. The last chunk to finish queues `text_assemble`, which runs the ordinary import
//! with every page cached, so the text is the same as a single pass. A worker that dies in a
//! chunk leaves its finished pages cached, and the retry reads only the rest.
//! `wt text import --estimate` measures before anything is queued.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use wt_core::objects::ObjectStore;
use wt_core::records::sha256_hex;
use wt_db::job::JobKind;
use wt_ml::Embedder;

use super::manifest::{CorpusManifest, Format};
use super::pdf::tools::PdfTools;
use super::pdf::{self, OcrMode, PageCache, ReadOptions};
use super::{ImportOptions, ImportReport, PdfEnv, TextError};

/// Where queued imports keep their manifest, sources and plan, so a worker on any machine can
/// run them.
const IMPORTS: &str = "texts-work/_imports";

/// Counts fetches, to name their scratch folders apart.
static FETCHES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A range of pages of one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    /// The file's name beside the manifest.
    pub file: String,
    /// Its hash, which keys the page cache.
    pub sha256: String,
    /// The first page.
    pub first: u32,
    /// The last page.
    pub last: u32,
}

/// Every chunk of a manifest's PDFs.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Plan {
    /// The chunks, file by file.
    pub chunks: Vec<Chunk>,
    /// Pages to read, skipped pages left out.
    pub pages: u32,
}

/// Splits each PDF of the manifest into chunks of `chunk_pages`, leaving out skipped pages.
pub fn plan(tools: &dyn PdfTools, m: &CorpusManifest, dir: &Path) -> Result<Plan, TextError> {
    let settings = m.pdf.clone().unwrap_or_default();
    let skip = pdf::skipped(&settings.skip_pages)?;
    let mut out = Plan::default();
    for name in &m.source.files {
        let path = dir.join(name);
        let bytes = std::fs::read(&path)
            .map_err(|e| TextError::Source(format!("{}: {e}", path.display())))?;
        let sha256 = sha256_hex(&bytes);
        let count = tools.page_count(&path)?;
        let wanted: Vec<u32> = (1..=count)
            .filter(|n| !skip.iter().any(|&(a, b)| (a..=b).contains(n)))
            .collect();
        out.pages += u32::try_from(wanted.len()).unwrap_or(u32::MAX);
        let mut first = 1;
        while first <= count {
            let last = (first + settings.chunk_pages - 1).min(count);
            if wanted.iter().any(|n| (first..=last).contains(n)) {
                out.chunks.push(Chunk {
                    file: name.clone(),
                    sha256: sha256.clone(),
                    first,
                    last,
                });
            }
            first = last + 1;
        }
    }
    Ok(out)
}

impl Plan {
    /// Whether every chunk is done.
    pub fn done(&self, objects: &dyn ObjectStore, m: &CorpusManifest) -> Result<bool, TextError> {
        let settings = m.pdf.clone().unwrap_or_default();
        for c in &self.chunks {
            if !PageCache::new(objects, &c.sha256, &settings).chunk_done(c.first, c.last)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Whether the manifest is large enough to read in chunks.
    pub fn large(&self, m: &CorpusManifest) -> bool {
        self.pages > m.pdf.clone().unwrap_or_default().chunk_pages
    }
}

/// An `ocr_chunk` job's input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkJob {
    /// The queued import.
    pub bundle: String,
    /// The range.
    pub chunk: Chunk,
    /// Who imported it.
    pub by: String,
}

/// A `text_assemble` job's input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssembleJob {
    /// The queued import.
    pub bundle: String,
    /// Who imported it.
    pub by: String,
}

/// What queueing did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Queued {
    /// The queued import's id.
    pub bundle: String,
    /// Chunks in the plan.
    pub chunks: usize,
    /// Chunks newly queued; the rest were queued or done before.
    pub newly_queued: usize,
    /// Pages to read.
    pub pages: u32,
}

/// Stores the manifest, its sources and the plan, and queues one `ocr_chunk` job per chunk
/// not yet done.
pub async fn queue(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    m: &CorpusManifest,
    manifest_text: &str,
    dir: &Path,
    plan: &Plan,
    by: &str,
) -> Result<Queued, TextError> {
    let mut id_input = sha256_hex(manifest_text.as_bytes());
    for c in &plan.chunks {
        id_input.push_str(&c.sha256);
    }
    let bundle = sha256_hex(id_input.as_bytes());
    let prefix = format!("{IMPORTS}/{bundle}");
    objects.put(&format!("{prefix}/manifest.toml"), manifest_text.as_bytes())?;
    for name in &m.source.files {
        let path = dir.join(name);
        let bytes = std::fs::read(&path)
            .map_err(|e| TextError::Source(format!("{}: {e}", path.display())))?;
        objects.put(&format!("{prefix}/files/{name}"), &bytes)?;
    }
    let plan_json =
        serde_json::to_vec(plan).map_err(|e| TextError::Source(format!("the plan: {e}")))?;
    objects.put(&format!("{prefix}/plan.json"), &plan_json)?;
    let settings = m.pdf.clone().unwrap_or_default();
    let mut newly_queued = 0;
    for c in &plan.chunks {
        if PageCache::new(objects, &c.sha256, &settings).chunk_done(c.first, c.last)? {
            continue;
        }
        let payload = serde_json::to_value(ChunkJob {
            bundle: bundle.clone(),
            chunk: c.clone(),
            by: by.to_owned(),
        })
        .map_err(|e| TextError::Source(format!("a chunk job: {e}")))?;
        // `ocr:<bundle>:<pdf sha256>:<first>-<last>`, the spec's key with the queued import
        // standing for the edition, since a manifest with parts holds several.
        let key = format!("ocr:{bundle}:{}:{}-{}", c.sha256, c.first, c.last);
        if wt_db::job::enqueue(conn, JobKind::OcrChunk, None, &key, &payload)
            .await?
            .is_some()
        {
            newly_queued += 1;
        }
    }
    Ok(Queued {
        bundle,
        chunks: plan.chunks.len(),
        newly_queued,
        pages: plan.pages,
    })
}

/// A queued import fetched into scratch: its manifest and sources, deleted when dropped.
struct Fetched {
    dir: PathBuf,
    manifest: PathBuf,
    plan: Plan,
    m: CorpusManifest,
}

impl Drop for Fetched {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Copies a queued import's manifest, plan and source files (or only `only_file`) from the object
/// store into a scratch directory of its own, unique per fetch since one process may run several
/// chunks of an import at once.
fn fetch(
    objects: &dyn ObjectStore,
    bundle: &str,
    scratch: &Path,
    only_file: Option<&str>,
) -> Result<Fetched, TextError> {
    let prefix = format!("{IMPORTS}/{bundle}");
    let missing = |what: &str| TextError::Source(format!("queued import {bundle} has no {what}"));
    let manifest_text = objects
        .get(&format!("{prefix}/manifest.toml"))?
        .ok_or_else(|| missing("manifest"))?;
    let plan: Plan = serde_json::from_slice(
        &objects
            .get(&format!("{prefix}/plan.json"))?
            .ok_or_else(|| missing("plan"))?,
    )
    .map_err(|e| TextError::Source(format!("the plan of {bundle}: {e}")))?;
    let text = String::from_utf8_lossy(&manifest_text).into_owned();
    let m = CorpusManifest::parse(&text)?;
    // Unique per fetch: workers in one process may run chunks of the same import at once.
    let dir = scratch.join(format!(
        "import-{}-{}-{}",
        &bundle[..12.min(bundle.len())],
        std::process::id(),
        FETCHES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir)
        .map_err(|e| TextError::Source(format!("{}: {e}", dir.display())))?;
    let fetched = Fetched {
        manifest: dir.join("manifest.toml"),
        dir,
        plan,
        m,
    };
    std::fs::write(&fetched.manifest, &manifest_text)
        .map_err(|e| TextError::Source(format!("{}: {e}", fetched.manifest.display())))?;
    for name in &fetched.m.source.files {
        if only_file.is_some_and(|f| f != name) {
            continue;
        }
        let bytes = objects
            .get(&format!("{prefix}/files/{name}"))?
            .ok_or_else(|| missing(name))?;
        let path = fetched.dir.join(name);
        std::fs::write(&path, bytes)
            .map_err(|e| TextError::Source(format!("{}: {e}", path.display())))?;
    }
    Ok(fetched)
}

/// What a chunk job did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChunkOutcome {
    /// Pages read now; cached pages are not read again.
    pub pages_read: usize,
    /// Whether this chunk was the last, so assembly is queued.
    pub assemble_queued: bool,
}

/// Runs one `ocr_chunk` job: reads its pages one at a time into the cache, marks the chunk
/// done, and queues assembly when every chunk is.
pub async fn run_chunk(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    tools: &dyn PdfTools,
    scratch: &Path,
    job: &ChunkJob,
) -> Result<ChunkOutcome, TextError> {
    let fetched = fetch(objects, &job.bundle, scratch, Some(&job.chunk.file))?;
    let settings = fetched.m.pdf.clone().unwrap_or_default();
    let cache = PageCache::new(objects, &job.chunk.sha256, &settings);
    let read = pdf::read(
        tools,
        &fetched.dir.join(&job.chunk.file),
        &settings,
        &fetched.dir.join("pages"),
        ReadOptions {
            cache: Some(&cache),
            pages: Some((job.chunk.first, job.chunk.last)),
            force_dpi: None,
        },
    )?;
    cache.mark_chunk(job.chunk.first, job.chunk.last)?;
    let assemble_queued = if fetched.plan.done(objects, &fetched.m)? {
        let payload = serde_json::to_value(AssembleJob {
            bundle: job.bundle.clone(),
            by: job.by.clone(),
        })
        .map_err(|e| TextError::Source(format!("the assemble job: {e}")))?;
        wt_db::job::enqueue(
            conn,
            JobKind::TextAssemble,
            None,
            &format!("assemble:{}", job.bundle),
            &payload,
        )
        .await?
        .is_some()
    } else {
        false
    };
    Ok(ChunkOutcome {
        pages_read: read.read_now,
        assemble_queued,
    })
}

/// Runs one `text_assemble` job: the ordinary import, which finds every page cached.
pub async fn run_assemble(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    embedder: &dyn Embedder,
    env: &PdfEnv<'_>,
    allowed_codes: &[String],
    job: &AssembleJob,
) -> Result<ImportReport, TextError> {
    let fetched = fetch(objects, &job.bundle, &env.scratch, None)?;
    if !fetched.plan.done(objects, &fetched.m)? {
        return Err(TextError::Source(format!(
            "queued import {} still has unread chunks",
            job.bundle
        )));
    }
    let report = super::import_with(
        conn,
        objects,
        embedder,
        &fetched.manifest,
        &ImportOptions {
            allowed_codes,
            actor: &job.by,
            apply: true,
            force: false,
            pdf_env: Some(env),
        },
    )
    .await?;
    // The queued copy has served its purpose: the edition keeps its own.
    objects.delete_prefix(&format!("{IMPORTS}/{}/", job.bundle))?;
    Ok(report)
}

/// What `wt text import --estimate` prints.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Estimate {
    /// Pages to read, skipped pages left out.
    pub total_pages: u32,
    /// Pages without a usable text layer.
    pub ocr_pages: u32,
    /// Pages OCRed to measure the speed.
    pub sampled_pages: u32,
    /// Measured OCR pages per minute on this machine, one worker.
    pub pages_per_minute: f64,
    /// The concurrency assumed.
    pub concurrency: usize,
    /// Projected wall time for the OCR pages, in minutes.
    pub projected_minutes: f64,
    /// `ocr_chunk` jobs the import would queue.
    pub chunks: usize,
    /// The most pages the vision pass could send: none while it is deferred.
    pub vision_calls_worst_case: u32,
}

/// Pages sampled for the speed measurement.
const ESTIMATE_SAMPLE: u32 = 3;

/// Measures a PDF import without queueing anything: page counts, which pages need OCR, and the
/// speed of OCR on the first few of them.
pub fn estimate(
    tools: &dyn PdfTools,
    manifest_path: &Path,
    scratch: &Path,
    concurrency: usize,
) -> Result<Estimate, TextError> {
    let text = std::fs::read_to_string(manifest_path)
        .map_err(|e| TextError::Manifest(format!("reading {}: {e}", manifest_path.display())))?;
    let m = CorpusManifest::parse(&text)?;
    if m.source.format != Format::Pdf {
        return Err(TextError::Manifest("--estimate is for PDF sources".into()));
    }
    let dir = manifest_path.parent().unwrap_or(Path::new("."));
    let settings = m.pdf.clone().unwrap_or_default();
    let plan = plan(tools, &m, dir)?;
    let skip = pdf::skipped(&settings.skip_pages)?;
    std::fs::create_dir_all(scratch)
        .map_err(|e| TextError::Source(format!("{}: {e}", scratch.display())))?;
    let mut ocr: Vec<(PathBuf, u32)> = Vec::new();
    for name in &m.source.files {
        let path = dir.join(name);
        for n in 1..=tools.page_count(&path)? {
            if skip.iter().any(|&(a, b)| (a..=b).contains(&n)) {
                continue;
            }
            let needs = match settings.ocr {
                OcrMode::Always => true,
                OcrMode::Never => false,
                OcrMode::Auto => {
                    !pdf::usable_layer(&tools.text_layer(&path, n)?, settings.min_layer_chars)
                }
            };
            if needs {
                ocr.push((path.clone(), n));
            }
        }
    }
    let started = Instant::now();
    let mut sampled = 0_u32;
    for (path, n) in ocr.iter().take(ESTIMATE_SAMPLE as usize) {
        let png = scratch.join(format!("estimate-{n}.png"));
        let result = tools
            .render(path, *n, settings.dpi, None, &png)
            .and_then(|()| tools.ocr(&png, &settings.languages, *n));
        let _ = std::fs::remove_file(&png);
        result?;
        sampled += 1;
    }
    let minutes = started.elapsed().as_secs_f64().max(0.001) / 60.0;
    let pages_per_minute = if sampled == 0 {
        0.0
    } else {
        f64::from(sampled) / minutes
    };
    let ocr_pages = u32::try_from(ocr.len()).unwrap_or(u32::MAX);
    #[allow(clippy::cast_precision_loss)]
    let projected_minutes = if pages_per_minute > 0.0 {
        f64::from(ocr_pages) / (pages_per_minute * concurrency.max(1) as f64)
    } else {
        0.0
    };
    Ok(Estimate {
        total_pages: plan.pages,
        ocr_pages,
        sampled_pages: sampled,
        pages_per_minute,
        concurrency: concurrency.max(1),
        projected_minutes,
        chunks: if plan.large(&m) { plan.chunks.len() } else { 0 },
        vision_calls_worst_case: 0,
    })
}

/// `wt text reocr`: reads a page range of an edition's source again at `dpi`, replacing the
/// cached pages, and imports the edition again. Its units change, so it needs a new proofread.
#[allow(clippy::too_many_arguments)]
pub async fn reocr(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    embedder: &dyn Embedder,
    env: &PdfEnv<'_>,
    allowed_codes: &[String],
    edition: sqlx::types::Uuid,
    pages: (u32, u32),
    dpi: u32,
    by: &str,
) -> Result<ImportReport, TextError> {
    let info = wt_db::texts::edition_summary(conn, edition)
        .await?
        .ok_or_else(|| TextError::Source(format!("edition {edition} does not exist")))?;
    let work = super::work_prefix(&info.work_title, &info.edition_label);
    let source = super::source_prefix(&info.work_title, &info.edition_label);
    let manifest_text = objects
        .get(&format!("{work}/manifest.toml"))?
        .ok_or_else(|| {
            TextError::Source(format!(
                "edition {edition} has no stored manifest; import it again"
            ))
        })?;
    let m = CorpusManifest::parse(&String::from_utf8_lossy(&manifest_text))?;
    if m.source.format != Format::Pdf {
        return Err(TextError::Source("reocr is for PDF editions".into()));
    }
    let dir = env.scratch.join(format!("reocr-{edition}"));
    std::fs::create_dir_all(&dir)
        .map_err(|e| TextError::Source(format!("{}: {e}", dir.display())))?;
    let result = async {
        let manifest = dir.join("manifest.toml");
        std::fs::write(&manifest, &manifest_text)
            .map_err(|e| TextError::Source(format!("{}: {e}", manifest.display())))?;
        let settings = m.pdf.clone().unwrap_or_default();
        for name in &m.source.files {
            let bytes = objects
                .get(&format!("{source}/{name}"))?
                .ok_or_else(|| TextError::Source(format!("the stored source {name} is gone")))?;
            let path = dir.join(name);
            std::fs::write(&path, &bytes)
                .map_err(|e| TextError::Source(format!("{}: {e}", path.display())))?;
            let cache = PageCache::new(objects, &sha256_hex(&bytes), &settings);
            pdf::read(
                env.tools,
                &path,
                &settings,
                &dir.join("pages"),
                ReadOptions {
                    cache: Some(&cache),
                    pages: Some(pages),
                    force_dpi: Some(dpi),
                },
            )?;
        }
        super::import_with(
            conn,
            objects,
            embedder,
            &manifest,
            &ImportOptions {
                allowed_codes,
                actor: by,
                apply: true,
                force: true,
                pdf_env: Some(env),
            },
        )
        .await
    }
    .await;
    let _ = std::fs::remove_dir_all(&dir);
    result
}
