//! `wt text import` (Spec 08, Import pipeline): a corpus text from a manifest and its source
//! files into persons, a work, an edition, its units with embeddings and its locators. A dry
//! run reports and changes nothing; `--apply` writes in one transaction, and an unchanged
//! source and manifest make it a no-op.

pub mod extract;
pub mod manifest;
pub mod reference;
pub mod units;

use std::path::Path;

use serde::Serialize;
use wt_core::objects::ObjectStore;
use wt_core::records::sha256_hex;
use wt_db::texts::{self, NewEdition, NewUnit};
use wt_ml::Embedder;

use manifest::CorpusManifest;
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

/// What an import found and did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
/// only reports.
#[allow(clippy::too_many_arguments)]
pub async fn import(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    embedder: &dyn Embedder,
    manifest_path: &Path,
    allowed_codes: &[String],
    actor: &str,
    apply: bool,
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
    for name in &m.source.files {
        let path = dir.join(name);
        let bytes = std::fs::read(&path)
            .map_err(|e| TextError::Source(format!("{}: {e}", path.display())))?;
        key_input.push_str(&sha256_hex(&bytes));
        files.push((name.clone(), bytes));
        sections.extend(extract::read(&path, m.source.format)?);
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
    };
    // A dry run changes nothing in the database (Spec 08 acceptance), so it is not logged.
    if !apply {
        return Ok(report);
    }
    if texts::edition_hash(conn, &m.work.title, &m.edition.label).await? == Some(Some(key.clone()))
    {
        report.unchanged = true;
        return Ok(report);
    }

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
        },
    )
    .await?;
    let new_units: Vec<NewUnit<'_>> = units
        .iter()
        .zip(&vectors)
        .map(|(u, v)| NewUnit {
            ordinal: i32::try_from(u.ordinal).unwrap_or(i32::MAX),
            locator: &u.locator,
            label: &u.label,
            heading: u.heading.as_deref(),
            text: &u.text,
            word_count: i32::try_from(u.word_count).unwrap_or(i32::MAX),
            content_hash: sha256_hex(u.text.as_bytes()),
            embedding: vector_literal(v),
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
    let prefix = format!(
        "texts-source/{}/{}",
        slug(&m.work.title),
        slug(&m.edition.label)
    );
    for (name, bytes) in &files {
        objects.put(&format!("{prefix}/{name}"), bytes)?;
    }
    report.applied = true;
    Ok(report)
}
