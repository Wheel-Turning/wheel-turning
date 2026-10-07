//! `wt text proof`, `wt text proof-import` and `wt text fix` (Spec 08, PDFs and OCR, the
//! proofread gate). An edition serves only with a passing proofread of its current units; a fix
//! changes the units, so it needs a new one.

use std::collections::BTreeMap;
use std::path::Path;

use base64::Engine as _;
use serde::Serialize;
use sqlx::types::Uuid;
use wt_core::objects::ObjectStore;
use wt_core::records::sha256_hex;
use wt_db::texts::{self, NewProof};
use wt_ml::Embedder;

use super::pdf::proof::{self, Candidate, Group, ProofFile, SheetUnit, Verdict};
use super::pdf::tools::{Crop, PdfTools};
use super::pdf::{PLACEMENTS, Placements};
use super::{TextError, units, vector_literal};

/// The resolution of the page crops on a proof sheet.
const CROP_DPI: u32 = 150;
/// Space kept around a unit's box on its crop, as a share of the page.
const CROP_MARGIN: f32 = 0.02;

/// The thresholds of a proofread, from `[texts]` in `pipeline.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds {
    /// Errors allowed in the random units.
    pub random_max: u32,
    /// Errors allowed in the lowest-confidence units.
    pub worst_max: u32,
}

fn is_ocr(extraction: &str) -> bool {
    matches!(extraction, "ocr" | "ocr_vision")
}

async fn drawn(
    conn: &mut sqlx::PgConnection,
    edition: Uuid,
) -> Result<
    (
        texts::EditionSummary,
        Vec<texts::ProofUnit>,
        Vec<(Uuid, Group)>,
    ),
    TextError,
> {
    let info = texts::edition_summary(conn, edition)
        .await?
        .ok_or_else(|| TextError::Proof(format!("edition {edition} does not exist")))?;
    let units = texts::proof_units(conn, edition).await?;
    if units.is_empty() {
        return Err(TextError::Proof(format!("edition {edition} has no units")));
    }
    let candidates: Vec<Candidate> = units
        .iter()
        .map(|u| Candidate {
            id: u.id,
            ordinal: u.ordinal,
            confidence: u.confidence,
            source_page: u.source_page,
        })
        .collect();
    let hash = info.units_hash.clone().unwrap_or_default();
    let sample = proof::sample(
        &candidates,
        is_ocr(&info.extraction),
        proof::seed(edition, &hash),
    );
    Ok((info, units, sample))
}

/// The pixel rectangle of a box on a page rendered at `dpi`.
fn crop_of(frac: [f32; 4], w_pts: f32, h_pts: f32, dpi: u32) -> Crop {
    #[allow(clippy::cast_precision_loss)]
    let scale = dpi as f32 / 72.0;
    let (pw, ph) = (w_pts * scale, h_pts * scale);
    let x0 = (frac[0] - CROP_MARGIN).max(0.0) * pw;
    let y0 = (frac[1] - CROP_MARGIN).max(0.0) * ph;
    let x1 = (frac[2] + CROP_MARGIN).min(1.0) * pw;
    let y1 = (frac[3] + CROP_MARGIN).min(1.0) * ph;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Crop {
        x: x0 as u32,
        y: y0 as u32,
        w: (x1 - x0).max(1.0) as u32,
        h: (y1 - y0).max(1.0) as u32,
    }
}

/// The proof sheet for an edition, with its title. Page crops come from the stored source PDF,
/// rendered in `scratch` and deleted there; they are on the sheet only.
pub async fn sheet(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    tools: &dyn PdfTools,
    edition: Uuid,
    scratch: &Path,
) -> Result<(String, String), TextError> {
    let (info, units, sample) = drawn(conn, edition).await?;
    let prefix = super::source_prefix(&info.work_title, &info.edition_label);
    let placements: Option<Placements> = objects
        .get(&format!(
            "{}/{PLACEMENTS}",
            super::work_prefix(&info.work_title, &info.edition_label)
        ))?
        .map(|b| serde_json::from_slice(&b))
        .transpose()
        .map_err(|e| TextError::Proof(format!("the stored placements: {e}")))?;
    std::fs::create_dir_all(scratch)
        .map_err(|e| TextError::Source(format!("{}: {e}", scratch.display())))?;
    let mut pdfs: BTreeMap<String, std::path::PathBuf> = BTreeMap::new();
    let mut out = Vec::new();
    let by_id: BTreeMap<Uuid, &texts::ProofUnit> = units.iter().map(|u| (u.id, u)).collect();
    let result = async {
        for (id, group) in &sample {
            let Some(u) = by_id.get(id) else { continue };
            let mut crop = None;
            if let Some(p) = placements
                .as_ref()
                .and_then(|p| p.units.get(&u.locator).map(|x| (p, x)))
            {
                let (all, place) = p;
                let file = all.files.get(place.file).cloned().unwrap_or_default();
                let local = match pdfs.get(&file) {
                    Some(path) => path.clone(),
                    None => {
                        let bytes = objects.get(&format!("{prefix}/{file}"))?.ok_or_else(|| {
                            TextError::Proof(format!("the stored source {file} is gone"))
                        })?;
                        let path = scratch.join(format!("proof-{}.pdf", pdfs.len()));
                        std::fs::write(&path, bytes)
                            .map_err(|e| TextError::Source(format!("{}: {e}", path.display())))?;
                        pdfs.insert(file.clone(), path.clone());
                        path
                    }
                };
                let png = scratch.join(format!("crop-{id}.png"));
                let rect = crop_of(place.frac, place.w_pts, place.h_pts, CROP_DPI);
                let rendered = tools.render(&local, place.page, CROP_DPI, Some(rect), &png);
                let bytes = rendered.and_then(|()| {
                    std::fs::read(&png)
                        .map_err(|e| TextError::Source(format!("{}: {e}", png.display())))
                });
                let _ = std::fs::remove_file(&png);
                crop = Some(base64::engine::general_purpose::STANDARD.encode(bytes?));
            }
            out.push(SheetUnit {
                id: *id,
                group: *group,
                label: u.label.clone(),
                text: u.text.clone(),
                page: u.source_page,
                crop_png_base64: crop,
            });
        }
        Ok::<(), TextError>(())
    }
    .await;
    for path in pdfs.values() {
        let _ = std::fs::remove_file(path);
    }
    result?;
    let title = format!("{} ({})", info.work_title, info.edition_label);
    let html = proof::sheet(
        &title,
        edition,
        info.units_hash.as_deref().unwrap_or(""),
        &out,
    );
    Ok((title, html))
}

/// What `proof-import` recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProofReport {
    /// The edition.
    pub edition: Uuid,
    /// The outcome.
    pub verdict: Verdict,
    /// Whether the edition now serves, given its evidence and source.
    pub ready: bool,
    /// Citations linked to its units after a pass.
    pub citations_linked: u64,
}

/// Records a proofread from the sheet's file. A pass makes the edition ready; a failure lists
/// the error notes.
pub async fn import_proof(
    conn: &mut sqlx::PgConnection,
    edition: Uuid,
    file: &ProofFile,
    reviewer: &str,
    thresholds: Thresholds,
) -> Result<ProofReport, TextError> {
    if reviewer.trim().is_empty() {
        return Err(TextError::Proof("a reviewer name is required".into()));
    }
    if file.edition_id != edition {
        return Err(TextError::Proof(format!(
            "the file is for edition {}, not {edition}",
            file.edition_id
        )));
    }
    let (info, _, sample) = drawn(conn, edition).await?;
    if info.units_hash.as_deref() != Some(file.units_hash.as_str()) {
        return Err(TextError::Proof(
            "the edition's units changed since the sheet was made; run wt text proof again".into(),
        ));
    }
    let verdict = proof::score(
        &sample,
        &file.answers,
        thresholds.random_max,
        thresholds.worst_max,
    )?;
    let mut tx = sqlx::Acquire::begin(&mut *conn).await?;
    texts::record_proof(
        &mut tx,
        &NewProof {
            edition,
            units_hash: &file.units_hash,
            reviewer,
            passed: verdict.passed,
            random_sampled: verdict.random_sampled,
            random_errors: verdict.random_errors,
            worst_sampled: verdict.worst_sampled,
            worst_errors: verdict.worst_errors,
            notes: (!verdict.notes.is_empty()).then(|| verdict.notes.join("\n")),
        },
    )
    .await?;
    let ready = verdict.passed && texts::set_state(&mut tx, edition, "ready").await?;
    tx.commit().await?;
    let citations_linked = if ready {
        wt_db::reference::link_citations(conn).await?
    } else {
        0
    };
    Ok(ProofReport {
        edition,
        verdict,
        ready,
        citations_linked,
    })
}

/// What `wt text fix` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FixReport {
    /// The unit.
    pub unit: Uuid,
    /// Its edition, which now needs a new proofread.
    pub edition: Uuid,
    /// Its locator.
    pub locator: String,
}

/// Corrects a unit's text, logs the change, re-embeds it and returns its edition to
/// `imported` until a new proofread passes.
pub async fn fix_unit(
    conn: &mut sqlx::PgConnection,
    embedder: &dyn Embedder,
    unit: Uuid,
    text: &str,
    actor: &str,
    reason: &str,
) -> Result<FixReport, TextError> {
    let text = super::extract::normalize(text);
    let chars = text.chars().count();
    if !(units::MIN_CHARS..=units::MAX_CHARS).contains(&chars) {
        return Err(TextError::Proof(format!(
            "a unit holds {} to {} characters; this text has {chars}",
            units::MIN_CHARS,
            units::MAX_CHARS
        )));
    }
    let vector = embedder.embed(std::slice::from_ref(&text)).await?;
    let embedding = vector
        .first()
        .map(|v| vector_literal(v))
        .ok_or_else(|| TextError::Proof("the embedder returned nothing".into()))?;
    let mut tx = sqlx::Acquire::begin(&mut *conn).await?;
    let fixed = texts::fix_unit(
        &mut tx,
        &texts::UnitFix {
            unit,
            text: &text,
            word_count: i32::try_from(text.split_whitespace().count()).unwrap_or(i32::MAX),
            content_hash: sha256_hex(text.as_bytes()),
            embedding,
            model: embedder.model_id(),
            actor,
            reason,
        },
    )
    .await?
    .ok_or_else(|| TextError::Proof(format!("unit {unit} does not exist")))?;
    tx.commit().await?;
    Ok(FixReport {
        unit,
        edition: fixed.0,
        locator: fixed.1,
    })
}
