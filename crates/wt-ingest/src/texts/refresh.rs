//! `wt text refresh --edition <id>` (Spec 08, Ingesting from URLs, refreshing): a conditional
//! request for the edition's source. An unchanged document is a no-op. A changed one is stored
//! and compared unit by unit with what serves; serving continues unchanged until `--apply`
//! imports the new version, which then needs its proofread like any other.

use std::collections::BTreeMap;

use serde::Serialize;
use sqlx::types::Uuid;
use url::Url;
use wt_core::objects::ObjectStore;
use wt_ml::Embedder;

use super::takedown::stored_files;
use super::url::Sidecar;
use super::web::{Got, Validators, WebFetcher};
use super::{ImportOptions, ImportReport, TextError, work_prefix};

/// What a refresh found.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RefreshReport {
    /// The edition.
    pub edition: Uuid,
    /// Its source.
    pub source_url: String,
    /// Whether the document changed.
    pub changed: bool,
    /// Units only in the new version, by locator.
    pub added: Vec<String>,
    /// Units only in the served version.
    pub removed: Vec<String>,
    /// Units whose text changed.
    pub changed_units: Vec<String>,
    /// The import, with `--apply`.
    pub import: Option<ImportReport>,
}

/// Refreshes an edition fetched from a URL. When the body changed it reports the diff against
/// what serves, and with `opts.apply` imports the new body. The import always runs with `force`,
/// whatever `opts` says, since the files on disk are new.
pub async fn refresh(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    embedder: &dyn Embedder,
    fetcher: &WebFetcher,
    edition: Uuid,
    opts: &ImportOptions<'_>,
) -> Result<RefreshReport, TextError> {
    let scope = wt_db::text_takedown::TextScope::Edition(edition);
    let e = wt_db::text_takedown::editions(conn, &scope)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| TextError::Source(format!("edition {edition} does not exist")))?;
    let source_url = e
        .source_url
        .clone()
        .ok_or_else(|| TextError::Source(format!("edition {edition} has no source URL")))?;
    let url =
        Url::parse(&source_url).map_err(|err| TextError::Source(format!("{source_url}: {err}")))?;
    let host = url.host_str().unwrap_or("unknown").to_owned();
    let files = stored_files(objects, &e)?;
    let file = files
        .first()
        .ok_or_else(|| TextError::Source(format!("edition {edition} lists no source file")))?;
    let sidecar: Option<Sidecar> = objects
        .get(&format!("texts-source/{host}/{}.json", file.sha256))?
        .and_then(|b| serde_json::from_slice(&b).ok());
    let validators = Validators {
        etag: sidecar.as_ref().and_then(|s| s.etag.clone()),
        last_modified: sidecar.as_ref().and_then(|s| s.last_modified.clone()),
    };
    let mut report = RefreshReport {
        edition,
        source_url: source_url.clone(),
        changed: false,
        added: Vec::new(),
        removed: Vec::new(),
        changed_units: Vec::new(),
        import: None,
    };
    let fetched = match fetcher.get(&url, &validators).await? {
        Got::NotModified => return Ok(report),
        Got::Body(b) if b.sha256 == file.sha256 => return Ok(report),
        Got::Body(b) => b,
    };
    report.changed = true;
    let raw_key = format!("texts-source/{host}/{}", fetched.sha256);
    objects.put(&raw_key, &fetched.bytes)?;
    let sidecar = Sidecar {
        url: fetched.url.clone(),
        final_url: fetched.final_url.clone(),
        fetched_at: chrono::Utc::now().to_rfc3339(),
        status: fetched.status,
        content_type: fetched.content_type.clone(),
        etag: fetched.etag.clone(),
        last_modified: fetched.last_modified.clone(),
        sha256: fetched.sha256.clone(),
    };
    let json =
        serde_json::to_vec_pretty(&sidecar).map_err(|err| TextError::Source(err.to_string()))?;
    objects.put(&format!("{raw_key}.json"), &json)?;

    // The stored manifest, with the new body in its file's place.
    let manifest = objects
        .get(&format!(
            "{}/manifest.toml",
            work_prefix(&e.work_title, &e.edition_label)
        ))?
        .ok_or_else(|| TextError::Source(format!("edition {edition} has no stored manifest")))?;
    let dir = std::env::temp_dir().join(format!("wt-refresh-{edition}-{}", &fetched.sha256[..12]));
    std::fs::create_dir_all(&dir)
        .map_err(|err| TextError::Source(format!("{}: {err}", dir.display())))?;
    let result = async {
        let manifest_path = dir.join("manifest.toml");
        std::fs::write(&manifest_path, &manifest)
            .map_err(|err| TextError::Source(format!("{}: {err}", manifest_path.display())))?;
        std::fs::write(dir.join(&file.name), &fetched.bytes)
            .map_err(|err| TextError::Source(format!("{}: {err}", file.name)))?;
        // A dry run first: the diff against what serves.
        let fake = wt_ml::FakeEmbedder::new("refresh-dry-run");
        let mut options = ImportOptions {
            apply: false,
            force: true,
            ..*opts
        };
        let draft = super::import(conn, objects, &fake, &manifest_path, &options).await?;
        let served: BTreeMap<String, String> = wt_db::texts::unit_digest(conn, edition)
            .await?
            .into_iter()
            .collect();
        let fresh: BTreeMap<String, String> = draft.digest.iter().cloned().collect();
        for (loc, hash) in &fresh {
            match served.get(loc) {
                None => report.added.push(loc.clone()),
                Some(h) if h != hash => report.changed_units.push(loc.clone()),
                Some(_) => {}
            }
        }
        report.removed = served
            .keys()
            .filter(|l| !fresh.contains_key(*l))
            .cloned()
            .collect();
        if !opts.apply {
            return Ok::<ImportReport, TextError>(draft);
        }
        options.apply = true;
        let applied = super::import(conn, objects, embedder, &manifest_path, &options).await?;
        wt_db::texts::set_fetch_info(conn, edition, &fetched.sha256, fetched.etag.as_deref())
            .await?;
        Ok(applied)
    }
    .await;
    let _ = std::fs::remove_dir_all(&dir);
    let draft = result?;
    report.import = Some(draft);
    Ok(report)
}
