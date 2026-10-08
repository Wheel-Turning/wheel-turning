//! `wt takedown --edition | --source | --url` for texts (Spec 08, Mandated sources, Easy
//! removal): the database part in one transaction, then every stored file of each edition, its
//! cached pages and its work files, checked gone afterwards. Also quarantine and release, which
//! are reversible, and the exclusion check every import runs first.

use serde::Serialize;
use sqlx::types::Uuid;
use wt_core::objects::ObjectStore;
use wt_db::text_takedown::{self as db, EditionRef, TextScope};

use super::{FILES, TextError, source_prefix, work_prefix};

/// A stored file of an edition, as `files.json` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct StoredFile {
    /// Its name beside the manifest.
    pub name: String,
    /// Its SHA-256.
    pub sha256: String,
}

/// The files an edition was imported from, from its work files.
pub fn stored_files(
    objects: &dyn ObjectStore,
    e: &EditionRef,
) -> Result<Vec<StoredFile>, TextError> {
    objects
        .get(&format!(
            "{}/{FILES}",
            work_prefix(&e.work_title, &e.edition_label)
        ))?
        .map(|b| serde_json::from_slice(&b))
        .transpose()
        .map_err(|err| TextError::Source(format!("files.json of {}: {err}", e.id)))
        .map(Option::unwrap_or_default)
}

/// What a takedown found or did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TakedownReport {
    /// The editions in scope.
    pub editions: Vec<EditionRef>,
    /// Units each held, or removed.
    pub units: i64,
    /// Citations unlinked.
    pub citations_unlinked: i64,
    /// Exclusions added.
    pub exclusions: usize,
    /// Stored objects deleted.
    pub objects_deleted: usize,
    /// Whether this was a dry run.
    pub dry_run: bool,
}

/// Takes texts down. A dry run counts and changes nothing.
pub async fn take_down(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    scope: &TextScope,
    reason: &str,
    actor: &str,
    dry_run: bool,
) -> Result<TakedownReport, TextError> {
    let editions = db::editions(conn, scope).await?;
    let mut report = TakedownReport {
        editions: editions.clone(),
        units: 0,
        citations_unlinked: 0,
        exclusions: 0,
        objects_deleted: 0,
        dry_run,
    };
    if dry_run {
        for e in &editions {
            report.units += db::unit_count(conn, e.id).await?;
        }
        return Ok(report);
    }
    let files: Vec<Vec<StoredFile>> = editions
        .iter()
        .map(|e| stored_files(objects, e))
        .collect::<Result<_, _>>()?;
    let mut tx = sqlx::Acquire::begin(&mut *conn).await?;
    for (e, fs) in editions.iter().zip(&files) {
        let hashes: Vec<String> = fs.iter().map(|f| f.sha256.clone()).collect();
        let removed = db::remove(&mut tx, e, &hashes, reason, actor)
            .await
            .map_err(|err| TextError::Source(format!("taking down {}: {err}", e.id)))?;
        report.units += removed.units;
        report.citations_unlinked += removed.citations_unlinked;
        report.exclusions += removed.exclusions.len();
    }
    match scope {
        TextScope::Source(key) => {
            report.exclusions += db::close_source(&mut tx, key, reason, actor)
                .await
                .map_err(|err| TextError::Source(format!("closing source {key}: {err}")))?
                .len();
        }
        TextScope::UrlPrefix(prefix) => {
            let active = wt_db::rights::active_exclusions(&mut tx)
                .await
                .map_err(|err| TextError::Source(err.to_string()))?;
            if wt_db::rights::exclusion_for_prefix(&active, prefix).is_none() {
                wt_db::rights::add_exclusion(&mut tx, "prefix", prefix, reason, None, actor)
                    .await
                    .map_err(|err| TextError::Source(err.to_string()))?;
                report.exclusions += 1;
            }
        }
        TextScope::Edition(_) => {}
    }
    tx.commit().await?;
    for (e, fs) in editions.iter().zip(&files) {
        report.objects_deleted += delete_objects(objects, e, fs)?;
    }
    Ok(report)
}

/// Deletes an edition's raw copies, work files and cached pages, then checks that none is
/// left. `files.json` goes last, since it is how the others are found.
fn delete_objects(
    objects: &dyn ObjectStore,
    e: &EditionRef,
    files: &[StoredFile],
) -> Result<usize, TextError> {
    let source = source_prefix(&e.work_title, &e.edition_label);
    let work = work_prefix(&e.work_title, &e.edition_label);
    let mut n = objects.delete_prefix(&format!("{source}/"))?;
    // A fetched document's raw copy and sidecar live under its host (Spec 08, Ingesting from
    // URLs, raw copy).
    let host = e
        .source_url
        .as_deref()
        .and_then(|u| url::Url::parse(u).ok())
        .and_then(|u| u.host_str().map(str::to_owned));
    for f in files {
        n += objects.delete_prefix(&format!("texts-work/_pages/{}/", f.sha256))?;
        if let Some(host) = &host {
            n += objects.delete_prefix(&format!("texts-source/{host}/{}", f.sha256))?;
        }
    }
    n += objects.delete_prefix(&format!("{work}/"))?;
    let mut left: Vec<String> = files
        .iter()
        .map(|f| format!("{source}/{}", f.name))
        .filter(|k| objects.get(k).ok().flatten().is_some())
        .collect();
    for key in [format!("{work}/{FILES}"), format!("{work}/placements.json")] {
        if objects.get(&key)?.is_some() {
            left.push(key);
        }
    }
    if !left.is_empty() {
        return Err(TextError::Source(format!(
            "objects remain after the takedown of {}: {}",
            e.id,
            left.join(", ")
        )));
    }
    Ok(n)
}

/// Quarantines an edition: it stops serving at once and keeps everything, until released.
pub async fn quarantine(
    conn: &mut sqlx::PgConnection,
    edition: Uuid,
    reason: &str,
    actor: &str,
) -> Result<String, TextError> {
    db::set_state_logged(conn, edition, "quarantined", reason, actor)
        .await?
        .ok_or_else(|| TextError::Source(format!("edition {edition} does not exist")))
}

/// Releases a quarantined edition: `ready` if its current units passed a proofread, else
/// `imported`. Returns the new state.
pub async fn release(
    conn: &mut sqlx::PgConnection,
    edition: Uuid,
    actor: &str,
) -> Result<String, TextError> {
    let state = if db::proofread_passed(conn, edition).await? {
        "ready"
    } else {
        "imported"
    };
    let old = db::set_state_logged(conn, edition, state, "released from quarantine", actor)
        .await?
        .ok_or_else(|| TextError::Source(format!("edition {edition} does not exist")))?;
    if old != "quarantined" {
        return Err(TextError::Source(format!(
            "edition {edition} was {old}, not quarantined"
        )));
    }
    Ok(state.to_owned())
}

/// What an import is about to bring in, checked against active exclusions.
pub struct Incoming<'a> {
    /// The work's title.
    pub work: &'a str,
    /// The edition's label.
    pub edition: &'a str,
    /// Where it came from.
    pub source_url: Option<&'a str>,
    /// The source files' hashes.
    pub file_hashes: &'a [String],
    /// The units' hashes.
    pub unit_hashes: &'a [String],
}

/// Refuses anything an active exclusion covers, citing it: the work, a source URL under an
/// excluded prefix or host, or any file or unit by content hash, so the same document is
/// refused at any address. A withdrawn edition of the same work and label is refused too.
pub async fn check_exclusions(
    conn: &mut sqlx::PgConnection,
    incoming: &Incoming<'_>,
) -> Result<(), TextError> {
    let active = wt_db::rights::active_exclusions(conn)
        .await
        .map_err(|e| TextError::Source(e.to_string()))?;
    let cite = |x: &wt_db::rights::Exclusion, what: &str| {
        TextError::Excluded(format!(
            "{what} matches exclusion {} ({} {:?}): {}",
            x.id, x.scope, x.pattern, x.reason
        ))
    };
    if let Some(x) = active
        .iter()
        .find(|x| x.scope == "work" && x.pattern.eq_ignore_ascii_case(incoming.work))
    {
        return Err(cite(x, &format!("the work {:?}", incoming.work)));
    }
    if let Some(url) = incoming.source_url
        && let Some(x) = wt_db::rights::exclusion_for_url(&active, url)
    {
        return Err(cite(x, &format!("the source {url}")));
    }
    for h in incoming.file_hashes.iter().chain(incoming.unit_hashes) {
        if let Some(x) = active
            .iter()
            .find(|x| x.scope == "content" && x.pattern == *h)
        {
            return Err(cite(x, "a file or unit"));
        }
    }
    if db::existing_state(conn, incoming.work, incoming.edition)
        .await?
        .as_deref()
        == Some("withdrawn")
    {
        return Err(TextError::Excluded(format!(
            "{} ({}) was taken down; its stub stays so it is not brought in again",
            incoming.work, incoming.edition
        )));
    }
    Ok(())
}
