//! `wt text fetch --crawl` (Spec 08, Ingesting from URLs): an index page's links, filtered by
//! `--follow-pattern` and by the evidence scope, up to `--max-pages`, listed for a person to
//! choose from. Nothing is imported: the list is written with every line commented out, and
//! the chosen lines go to `wt text fetch --urls-file`.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::Serialize;
use url::Url;

use super::convert;
use super::web::{Got, Validators, WebFetcher};
use super::{TextError, slug};

/// What a crawl found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CrawlReport {
    /// Where it started.
    pub start: String,
    /// Index pages read.
    pub pages_read: usize,
    /// Documents in scope that match the pattern, for a person to choose from.
    pub candidates: Vec<String>,
    /// Links no evidence or source covers, listed and not fetched.
    pub outside_scope: Vec<String>,
    /// Links refused by an exclusion or `robots.txt`, with why.
    pub refused: Vec<(String, String)>,
    /// The selection file.
    pub list: Option<PathBuf>,
    /// Why it stopped early.
    pub stopped: Option<String>,
}

/// Crawl settings.
pub struct Crawl<'a> {
    /// The index page.
    pub start: &'a str,
    /// How many link levels below the index to read.
    pub depth: u32,
    /// Links that are documents.
    pub follow: Option<&'a Regex>,
    /// The most candidates listed.
    pub max_pages: usize,
    /// Where the selection file goes: `data-inbox/_crawl/`.
    pub inbox: &'a Path,
}

/// A URL without its fragment, so one page is crawled once.
fn clean(mut u: Url) -> Url {
    u.set_fragment(None);
    u
}

/// Compiles a `--follow-pattern`.
pub fn pattern(text: &str) -> Result<Regex, TextError> {
    Regex::new(text).map_err(|e| TextError::Manifest(format!("--follow-pattern: {e}")))
}

/// The evidence or source that covers a URL, if any, matched on its normalized form.
async fn covered(
    conn: &mut sqlx::PgConnection,
    u: &Url,
) -> Result<Option<wt_db::texts::Covering>, TextError> {
    let normalized = wt_db::rights::normalize_url(u.as_str());
    Ok(wt_db::texts::covering_evidence(conn, &normalized).await?)
}

/// Crawls an index.
pub async fn crawl(
    conn: &mut sqlx::PgConnection,
    fetcher: &WebFetcher,
    c: &Crawl<'_>,
) -> Result<CrawlReport, TextError> {
    let start =
        clean(Url::parse(c.start).map_err(|e| TextError::Source(format!("{}: {e}", c.start)))?);
    let mut report = CrawlReport {
        start: start.to_string(),
        pages_read: 0,
        candidates: Vec::new(),
        outside_scope: Vec::new(),
        refused: Vec::new(),
        list: None,
        stopped: None,
    };
    if covered(conn, &start).await?.is_none() {
        report.stopped = Some(
            "no approved evidence or enabled source covers the index, so it was not read".into(),
        );
        return Ok(report);
    }
    let mut seen: BTreeSet<String> = BTreeSet::from([start.to_string()]);
    let mut queue: VecDeque<(Url, u32)> = VecDeque::from([(start.clone(), 0)]);
    while let Some((page, level)) = queue.pop_front() {
        if report.candidates.len() >= c.max_pages {
            break;
        }
        let body = match fetcher.get(&page, &Validators::default()).await {
            Ok(Got::Body(b)) => b,
            Ok(Got::NotModified) => continue,
            Err(e) => {
                report.refused.push((page.to_string(), e.to_string()));
                continue;
            }
        };
        report.pages_read += 1;
        let html = String::from_utf8_lossy(&body.bytes);
        let base = Url::parse(&body.final_url).unwrap_or(page.clone());
        for href in convert::links(&html) {
            let Ok(link) = base.join(&href).map(clean) else {
                continue;
            };
            if !matches!(link.scheme(), "http" | "https") || !seen.insert(link.to_string()) {
                continue;
            }
            let in_scope = covered(conn, &link).await?.is_some();
            let document = c.follow.is_none_or(|re| re.is_match(link.as_str()));
            if document {
                if !in_scope {
                    report.outside_scope.push(link.to_string());
                } else if report.candidates.len() < c.max_pages {
                    report.candidates.push(link.to_string());
                }
            } else if in_scope && level + 1 < c.depth.max(1) {
                queue.push_back((link, level + 1));
            }
        }
    }
    let dir = c.inbox.join("_crawl");
    std::fs::create_dir_all(&dir)
        .map_err(|e| TextError::Source(format!("{}: {e}", dir.display())))?;
    let name = format!(
        "{}-{}.txt",
        slug(start.host_str().unwrap_or("index")),
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    );
    let path = dir.join(name);
    let mut text = format!(
        "# Crawl of {} on {}.\n# Remove the `# ` before each document to import, then run\n#   wt text fetch --urls-file {}\n",
        start,
        chrono::Utc::now().format("%Y-%m-%d"),
        path.display()
    );
    for u in &report.candidates {
        text.push_str(&format!("# {u}\n"));
    }
    std::fs::write(&path, text)
        .map_err(|e| TextError::Source(format!("{}: {e}", path.display())))?;
    report.list = Some(path);
    Ok(report)
}
