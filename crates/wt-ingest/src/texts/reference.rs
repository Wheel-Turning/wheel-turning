//! `wt reference import` (Spec 08, Import pipeline): glossaries, alias lists, lineage files,
//! locator lists and chant metadata, from CSV with a column map in the manifest. Reference
//! assets feed the pipeline and are never shown. A dry run reports and changes nothing; an
//! applied asset is recorded by hash, so the same files and manifest again are a no-op.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use wt_core::objects::ObjectStore;
use wt_core::records::sha256_hex;
use wt_db::reference::{self, NewAsset, NewLocator};
use wt_registry::glossary::{GlossaryTerm, TermKind};

use super::TextError;

/// A CSV row, field name to value.
type Row = BTreeMap<String, String>;
/// A source file's name and bytes.
type SourceFile = (String, Vec<u8>);
/// A locator: its key, label, aliases and link-out address.
type LocatorRow = (String, String, Vec<String>, Option<String>);

/// A reference asset's manifest.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceManifest {
    /// Always `reference`.
    pub role: String,
    /// The asset.
    pub asset: AssetSection,
    /// The files.
    pub source: ReferenceSource,
}

/// `[asset]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetSection {
    /// What it is.
    pub kind: AssetKind,
    /// Its title.
    pub title: String,
    /// Where it came from.
    #[serde(default)]
    pub source_url: Option<String>,
    /// Approved evidence; omitted for material the founder compiled personally.
    #[serde(default)]
    pub license_evidence_id: Option<String>,
}

/// The kinds of reference asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    /// Canonical spellings and their mis-hearings.
    Glossary,
    /// People and their other names; never teacher aliases.
    AliasList,
    /// Who taught or transmitted to whom.
    Lineage,
    /// Hand-listed citation locators.
    LocatorList,
    /// Kept, with no table yet: chant tooling is in the backlog.
    ChantMetadata,
}

impl AssetKind {
    /// The asset kind as the report and the stored asset row name it.
    fn as_str(self) -> &'static str {
        match self {
            AssetKind::Glossary => "glossary",
            AssetKind::AliasList => "alias_list",
            AssetKind::Lineage => "lineage",
            AssetKind::LocatorList => "locator_list",
            AssetKind::ChantMetadata => "chant_metadata",
        }
    }
}

/// `[source]`: CSV files and which header holds each field.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceSource {
    /// File names beside the manifest.
    pub files: Vec<String>,
    /// Always `csv` in this build.
    #[serde(default = "csv")]
    pub format: String,
    /// Field name to CSV header, such as `canonical = "term"`. A field not mapped is read from
    /// the header of the same name.
    #[serde(default)]
    pub columns: BTreeMap<String, String>,
}

/// `format`'s default.
fn csv() -> String {
    "csv".into()
}

/// What an import found and did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReferenceReport {
    /// The asset's kind.
    pub kind: String,
    /// Rows read from the files.
    pub rows: usize,
    /// What was, or would be, written, per kind of row.
    pub counts: BTreeMap<String, usize>,
    /// Rows left as stored or skipped, with why.
    pub conflicts: Vec<String>,
    /// Teachers linked to a person by exact name, with `--link-teachers`.
    pub teachers_linked: Vec<String>,
    /// Whether anything was written.
    pub applied: bool,
    /// Whether the asset was already imported from these files and this manifest.
    pub unchanged: bool,
}

/// The rows of every file, each as field name to value.
fn read_rows(
    dir: &Path,
    source: &ReferenceSource,
) -> Result<(Vec<Row>, Vec<SourceFile>), TextError> {
    if source.format != "csv" {
        return Err(TextError::Manifest(format!(
            "reference format {:?}; this build reads csv",
            source.format
        )));
    }
    let mut rows = Vec::new();
    let mut files = Vec::new();
    for name in &source.files {
        let path = dir.join(name);
        let bytes = std::fs::read(&path)
            .map_err(|e| TextError::Source(format!("{}: {e}", path.display())))?;
        let mut reader = ::csv::ReaderBuilder::new()
            .trim(::csv::Trim::All)
            .from_reader(bytes.as_slice());
        let headers = reader
            .headers()
            .map_err(|e| TextError::Source(format!("{name}: {e}")))?
            .clone();
        // Header to field name, through the manifest's column map.
        let field_of = |header: &str| {
            source
                .columns
                .iter()
                .find(|(_, h)| h.as_str() == header)
                .map_or_else(|| header.to_owned(), |(f, _)| f.clone())
        };
        for record in reader.records() {
            let record = record.map_err(|e| TextError::Source(format!("{name}: {e}")))?;
            rows.push(
                headers
                    .iter()
                    .zip(record.iter())
                    .map(|(h, v)| (field_of(h), v.to_owned()))
                    .collect(),
            );
        }
        files.push((name.clone(), bytes));
    }
    Ok((rows, files))
}

/// A `;`-separated cell as a list, empty items left out.
fn list(value: Option<&String>) -> Vec<String> {
    value
        .map(|v| {
            v.split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// A row's field, refusing the row when it is missing or empty.
fn required<'a>(row: &'a BTreeMap<String, String>, field: &str) -> Result<&'a str, TextError> {
    row.get(field)
        .map(String::as_str)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| TextError::Source(format!("a row has no {field}")))
}

/// A glossary row's `kind`: `name`, `text_title` (or `title`), or a term otherwise.
fn term_kind(value: Option<&String>) -> TermKind {
    match value.map(|v| v.to_ascii_lowercase()).as_deref() {
        Some("name") => TermKind::Name,
        Some("text_title" | "title") => TermKind::TextTitle,
        _ => TermKind::Term,
    }
}

/// Runs a reference import. `link_teachers` links teachers whose display names match a lineage
/// person's exactly.
pub async fn import(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    manifest_path: &Path,
    allowed_codes: &[String],
    actor: &str,
    apply: bool,
    link_teachers: bool,
) -> Result<(ReferenceReport, Vec<GlossaryTerm>), TextError> {
    let manifest_text = std::fs::read_to_string(manifest_path)
        .map_err(|e| TextError::Manifest(format!("reading {}: {e}", manifest_path.display())))?;
    let m: ReferenceManifest =
        toml::from_str(&manifest_text).map_err(|e| TextError::Manifest(e.to_string()))?;
    if m.role != "reference" {
        return Err(TextError::Manifest(format!(
            "role is {:?}; `wt reference import` reads reference manifests",
            m.role
        )));
    }
    if manifest_text.contains("REPLACE_ME") {
        return Err(TextError::Manifest(
            "the manifest still holds REPLACE_ME placeholders".into(),
        ));
    }
    let evidence = match &m.asset.license_evidence_id {
        Some(id) => {
            let id: sqlx::types::Uuid = id
                .parse()
                .map_err(|e| TextError::Evidence(format!("license_evidence_id: {e}")))?;
            let ev = wt_db::texts::evidence(conn, id)
                .await?
                .ok_or_else(|| TextError::Evidence(format!("{id} does not exist")))?;
            if !ev.allowed || !allowed_codes.iter().any(|c| c == &ev.license_code) {
                return Err(TextError::Evidence(format!(
                    "{id} is not approved with an allowed code"
                )));
            }
            Some(id)
        }
        None => None,
    };
    let dir = manifest_path.parent().unwrap_or(Path::new("."));
    let (rows, files) = read_rows(dir, &m.source)?;
    let mut hash_input = sha256_hex(manifest_text.as_bytes());
    for (_, bytes) in &files {
        hash_input.push_str(&sha256_hex(bytes));
    }
    let sha = sha256_hex(hash_input.as_bytes());
    let mut report = ReferenceReport {
        kind: m.asset.kind.as_str().to_owned(),
        rows: rows.len(),
        ..ReferenceReport::default()
    };
    let mut glossary_terms = Vec::new();
    if m.asset.kind == AssetKind::Glossary {
        for row in &rows {
            glossary_terms.push(GlossaryTerm {
                canonical: required(row, "canonical")?.to_owned(),
                kind: term_kind(row.get("kind")),
                variants: list(row.get("variants")),
            });
        }
    }
    if !apply {
        report.counts.insert("rows".into(), rows.len());
        return Ok((report, Vec::new()));
    }
    if reference::asset_exists(conn, &sha).await? {
        report.unchanged = true;
        return Ok((report, Vec::new()));
    }

    let slug: String = m
        .asset
        .title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s3_key = format!("reference-assets/{slug}/{}", &sha[..12]);
    let mut tx = sqlx::Acquire::begin(&mut *conn).await?;
    let asset = reference::record_asset(
        &mut tx,
        &NewAsset {
            kind: m.asset.kind.as_str(),
            title: &m.asset.title,
            source_url: m.asset.source_url.as_deref(),
            evidence,
            s3_key: &s3_key,
            sha256: &sha,
            row_counts: serde_json::json!({}),
            imported_by: actor,
        },
    )
    .await?;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    match m.asset.kind {
        AssetKind::Glossary => {
            // Terms this asset adds record it; terms it only adds variants to keep their source.
            let existing: Vec<String> = wt_db::glossary::load(&mut tx)
                .await?
                .into_iter()
                .map(|t| t.canonical)
                .collect();
            let r = wt_db::glossary::import(&mut tx, &glossary_terms, actor).await?;
            let added: Vec<String> = glossary_terms
                .iter()
                .map(|t| t.canonical.trim().to_owned())
                .filter(|c| !existing.contains(c))
                .collect();
            reference::tag_glossary_terms(&mut tx, asset, &added).await?;
            counts.insert("terms_added".into(), r.added);
            counts.insert("variants_added".into(), r.variants_added);
            report.conflicts = r.conflicts;
        }
        AssetKind::AliasList => {
            let (mut persons, mut aliases) = (0, 0);
            for row in &rows {
                let name = required(row, "name")?;
                let kind = row
                    .get("kind")
                    .filter(|k| !k.is_empty())
                    .map_or("figure", String::as_str);
                let person = wt_db::texts::upsert_person(&mut tx, name, kind).await?;
                persons += 1;
                for alias in list(row.get("aliases")) {
                    match reference::add_person_alias(&mut tx, person, &alias).await? {
                        Some(holder) => report.conflicts.push(format!(
                            "{alias} already names {holder}; not added to {name}"
                        )),
                        None => aliases += 1,
                    }
                }
            }
            counts.insert("persons".into(), persons);
            counts.insert("aliases".into(), aliases);
        }
        AssetKind::Lineage => {
            let mut edges = 0;
            let mut persons = Vec::new();
            for row in &rows {
                let parent =
                    wt_db::texts::upsert_person(&mut tx, required(row, "parent")?, "figure")
                        .await?;
                let child =
                    wt_db::texts::upsert_person(&mut tx, required(row, "child")?, "figure").await?;
                let kind = row
                    .get("kind")
                    .filter(|k| !k.is_empty())
                    .map_or("teacher_student", String::as_str);
                if reference::add_lineage_edge(
                    &mut tx,
                    parent,
                    child,
                    kind,
                    asset,
                    row.get("note")
                        .map(String::as_str)
                        .filter(|n| !n.is_empty()),
                )
                .await?
                {
                    edges += 1;
                }
                persons.extend([parent, child]);
            }
            persons.sort_unstable();
            persons.dedup();
            counts.insert("persons".into(), persons.len());
            counts.insert("edges".into(), edges);
            if link_teachers {
                report.teachers_linked =
                    reference::link_teachers_exactly(&mut tx, &persons).await?;
            }
        }
        AssetKind::LocatorList => {
            let mut by_work: BTreeMap<String, Vec<LocatorRow>> = BTreeMap::new();
            for row in &rows {
                by_work
                    .entry(required(row, "work")?.to_owned())
                    .or_default()
                    .push((
                        required(row, "locator")?.to_owned(),
                        required(row, "label")?.to_owned(),
                        list(row.get("aliases")),
                        row.get("url").filter(|u| !u.is_empty()).cloned(),
                    ));
            }
            let mut n = 0;
            for (title, rows) in &by_work {
                let work = wt_db::texts::upsert_work(&mut tx, title, None, None).await?;
                let locators: Vec<NewLocator<'_>> = rows
                    .iter()
                    .map(|(locator, label, aliases, url)| NewLocator {
                        locator,
                        label,
                        aliases,
                        link_out_url: url.as_deref(),
                    })
                    .collect();
                n += reference::upsert_locators(&mut tx, work, &locators, Some(asset)).await?;
            }
            counts.insert("works".into(), by_work.len());
            counts.insert("locators".into(), n);
        }
        AssetKind::ChantMetadata => {
            counts.insert("rows_kept".into(), rows.len());
        }
    }
    reference::set_row_counts(&mut tx, asset, &serde_json::json!(counts)).await?;
    tx.commit().await?;
    for (name, bytes) in &files {
        objects.put(&format!("{s3_key}/{name}"), bytes)?;
    }
    report.counts = counts;
    report.applied = true;
    Ok((report, glossary_terms))
}

/// `wt text link-out`: a work read elsewhere, with its locators and no text, so talks can
/// cite it and the footnote links out. `aliases`, when given, name the whole work for
/// work-level citations (Spec 08).
pub async fn link_out(
    conn: &mut sqlx::PgConnection,
    title: &str,
    url: &str,
    locators_csv: Option<&Path>,
    aliases: &[String],
) -> Result<usize, TextError> {
    let rows = match locators_csv {
        Some(path) => {
            let source = ReferenceSource {
                files: vec![
                    path.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                ],
                format: "csv".into(),
                columns: BTreeMap::new(),
            };
            read_rows(path.parent().unwrap_or(Path::new(".")), &source)?.0
        }
        None => Vec::new(),
    };
    let mut tx = sqlx::Acquire::begin(&mut *conn).await?;
    let work = reference::upsert_link_out_work(&mut tx, title, url).await?;
    if !aliases.is_empty() {
        wt_db::locator::set_work_aliases(&mut tx, work, aliases).await?;
    }
    let parsed: Vec<LocatorRow> = rows
        .iter()
        .map(|row| {
            Ok((
                required(row, "locator")?.to_owned(),
                required(row, "label")?.to_owned(),
                list(row.get("aliases")),
                row.get("url").filter(|u| !u.is_empty()).cloned(),
            ))
        })
        .collect::<Result<_, TextError>>()?;
    let locators: Vec<NewLocator<'_>> = parsed
        .iter()
        .map(|(locator, label, aliases, u)| NewLocator {
            locator,
            label,
            aliases,
            link_out_url: u.as_deref().or(Some(url)),
        })
        .collect();
    let n = reference::upsert_locators(&mut tx, work, &locators, None).await?;
    tx.commit().await?;
    Ok(n)
}
