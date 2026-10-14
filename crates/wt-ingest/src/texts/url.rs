//! `wt text fetch` (Spec 08, Ingesting from URLs): a web address instead of a file. The gate
//! comes first: a URL that no approved evidence or enabled source covers gets a `HEAD` request
//! only, and nothing of its body is downloaded or stored. A covered URL is fetched politely,
//! its raw bytes kept under `texts-source/<host>/<sha256>` with a sidecar, its format sniffed,
//! and a manifest drafted in `data-inbox/texts/<slug>/` from what the document says about
//! itself. The draft then runs as a dry run, or, with `--yes` for a born-digital HTML page
//! with a clear title, as an import.

use std::path::{Path, PathBuf};

use serde::Serialize;
use url::Url;
use wt_core::objects::ObjectStore;
use wt_db::texts::Covering;
use wt_ml::Embedder;

use super::convert::{self, Kind, Meta};
use super::manifest::Format;
use super::web::{Got, Head, Validators, WebFetcher};
use super::{ImportReport, PdfEnv, TextError, slug};

/// How a fetch runs.
pub struct FetchOptions<'a> {
    /// `[licenses] allowed_license_codes`.
    pub allowed_codes: &'a [String],
    /// Who is fetching.
    pub actor: &'a str,
    /// Import a born-digital HTML page with a clear title without asking.
    pub yes: bool,
    /// Where drafts go: `data-inbox/`.
    pub inbox: &'a Path,
    /// For PDFs.
    pub pdf_env: Option<&'a PdfEnv<'a>>,
}

/// The sidecar kept beside a raw copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Sidecar {
    /// The address asked for.
    pub url: String,
    /// Where it ended.
    pub final_url: String,
    /// When.
    pub fetched_at: String,
    /// The status.
    pub status: u16,
    /// The content type.
    pub content_type: Option<String>,
    /// For a conditional refetch.
    pub etag: Option<String>,
    /// For a conditional refetch.
    pub last_modified: Option<String>,
    /// The body's hash.
    pub sha256: String,
}

/// What a fetch found and did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UrlReport {
    /// The address.
    pub url: String,
    /// The evidence or source that covers it, if any.
    pub covered_by: Option<Covering>,
    /// For an uncovered URL: what the `HEAD` request learned.
    pub head: Option<Head>,
    /// For a covered URL: the raw copy's key.
    pub raw_key: Option<String>,
    /// The format the bytes show.
    pub kind: Option<Kind>,
    /// What the document says about itself.
    pub meta: Option<Meta>,
    /// The drafted manifest.
    pub draft: Option<PathBuf>,
    /// The draft's dry run, or its import with `--yes`.
    pub import: Option<ImportReport>,
    /// Why it stopped short of an import.
    pub stopped: Option<String>,
}

impl UrlReport {
    /// A URL's report before anything is known about it.
    fn new(url: &str) -> Self {
        Self {
            url: url.to_owned(),
            covered_by: None,
            head: None,
            raw_key: None,
            kind: None,
            meta: None,
            draft: None,
            import: None,
            stopped: None,
        }
    }
}

/// The file extension a fetched document is stored under.
fn ext(format: Format) -> &'static str {
    match format {
        Format::Epub => "epub",
        Format::Html => "html",
        Format::Txt => "txt",
        Format::Md => "md",
        Format::Pdf => "pdf",
        Format::Docx => "docx",
    }
}

/// A format as the manifest writes it, which is its extension.
fn format_name(format: Format) -> &'static str {
    ext(format)
}

/// A TOML basic string on one line, for values written into a manifest.
fn toml_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' | '\r' | '\t' => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The manifest drafted from the document and the evidence. The translator is never guessed.
pub fn draft_manifest(
    final_url: &Url,
    meta: &Meta,
    covering: &Covering,
    file: &str,
    format: Format,
) -> String {
    let host = final_url.host_str().unwrap_or("source");
    let title = meta.title.clone().unwrap_or_else(|| {
        final_url
            .path_segments()
            .and_then(|mut s| s.next_back().map(str::to_owned))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| host.to_owned())
    });
    let label = meta
        .date
        .as_deref()
        .map(|d| d.chars().take(10).collect::<String>())
        .unwrap_or_else(|| host.to_owned());
    let rights_holder = covering
        .grantor
        .clone()
        .or_else(|| covering.publisher.clone())
        .unwrap_or_else(|| {
            if covering.license_code == "public-domain" {
                "public domain".to_owned()
            } else {
                host.to_owned()
            }
        });
    let mut m = String::from("role = \"corpus\"\n\n[work]\n");
    m.push_str(&format!("title = {}\n", toml_str(&title)));
    if let Some(a) = &meta.author {
        m.push_str(&format!("author = {}\n", toml_str(a)));
    }
    m.push_str("\n[edition]\n");
    m.push_str(&format!("label = {}\n", toml_str(&label)));
    m.push_str(
        "# translator = \"\"  # never guessed: name the translator if this is a translation\n",
    );
    m.push_str("language = \"en\"\n");
    m.push_str(&format!("source_url = {}\n", toml_str(final_url.as_str())));
    m.push_str(&format!("rights_holder = {}\n", toml_str(&rights_holder)));
    m.push_str(&format!(
        "license_evidence_id = \"{}\"\n",
        covering.evidence
    ));
    if let Some(key) = &covering.source_key {
        m.push_str(&format!("source_key = {}\n", toml_str(key)));
    }
    m.push_str(&format!(
        "\n[source]\nfiles = [{}]\nformat = \"{}\"\n",
        toml_str(file),
        format_name(format)
    ));
    let prefix = {
        let s = slug(&title);
        if s.is_empty() {
            "text".to_owned()
        } else {
            s.chars().take(40).collect()
        }
    };
    m.push_str(&format!(
        "\n[units]\nscheme = \"paragraph\"\nlocator_prefix = {}\nlabel_template = \"{{work}}, paragraph {{n}}\"\n",
        toml_str(&prefix)
    ));
    m
}

/// Fetches one URL through the gate and drafts its manifest.
pub async fn fetch_one(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    embedder: &dyn Embedder,
    fetcher: &WebFetcher,
    address: &str,
    opts: &FetchOptions<'_>,
) -> Result<UrlReport, TextError> {
    let mut report = UrlReport::new(address);
    let url = Url::parse(address).map_err(|e| TextError::Source(format!("{address}: {e}")))?;
    let normalized = wt_db::rights::normalize_url(url.as_str());
    let Some(covering) = wt_db::texts::covering_evidence(conn, &normalized).await? else {
        report.head = Some(fetcher.head(&url).await?);
        report.stopped = Some(
            "no approved evidence or enabled source covers this URL, so only a HEAD request \
             was sent and nothing was downloaded"
                .into(),
        );
        return Ok(report);
    };
    report.covered_by = Some(covering.clone());
    let Got::Body(fetched) = fetcher.get(&url, &Validators::default()).await? else {
        report.stopped = Some("the server answered 304 to an unconditional request".into());
        return Ok(report);
    };
    let final_url = Url::parse(&fetched.final_url)
        .map_err(|e| TextError::Source(format!("{}: {e}", fetched.final_url)))?;
    // The raw copy and its sidecar.
    let host = final_url.host_str().unwrap_or("unknown");
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
    let sidecar_json = serde_json::to_vec_pretty(&sidecar)
        .map_err(|e| TextError::Source(format!("the sidecar: {e}")))?;
    objects.put(&format!("{raw_key}.json"), &sidecar_json)?;
    report.raw_key = Some(raw_key);

    let kind = convert::sniff(&fetched.bytes, fetched.content_type.as_deref());
    report.kind = Some(kind.clone());
    let format = match kind {
        Kind::Readable(f) => f,
        Kind::Unsupported(why) => {
            report.stopped = Some(why);
            return Ok(report);
        }
    };

    // The draft folder, with the file named by its format.
    let file = format!("source.{}", ext(format));
    let staged = std::env::temp_dir().join(format!("wt-fetch-{}", fetched.sha256));
    std::fs::create_dir_all(&staged)
        .map_err(|e| TextError::Source(format!("{}: {e}", staged.display())))?;
    let staged_file = staged.join(&file);
    std::fs::write(&staged_file, &fetched.bytes)
        .map_err(|e| TextError::Source(format!("{}: {e}", staged_file.display())))?;
    let meta = match format {
        Format::Html => convert::html_meta(&String::from_utf8_lossy(&fetched.bytes)),
        Format::Docx => convert::docx_meta(&staged_file),
        _ => Meta::default(),
    };
    let _ = std::fs::remove_dir_all(&staged);
    let manifest = draft_manifest(&final_url, &meta, &covering, &file, format);
    let title_slug = slug(meta.title.as_deref().unwrap_or(host));
    let folder = opts.inbox.join("texts").join(if title_slug.is_empty() {
        fetched.sha256[..12].to_owned()
    } else {
        title_slug.chars().take(60).collect()
    });
    std::fs::create_dir_all(&folder)
        .map_err(|e| TextError::Source(format!("{}: {e}", folder.display())))?;
    std::fs::write(folder.join(&file), &fetched.bytes)
        .map_err(|e| TextError::Source(format!("{}: {e}", folder.display())))?;
    let manifest_path = folder.join("manifest.toml");
    std::fs::write(&manifest_path, &manifest)
        .map_err(|e| TextError::Source(format!("{}: {e}", manifest_path.display())))?;
    report.draft = Some(manifest_path.clone());
    let clear_title = meta
        .title
        .as_deref()
        .is_some_and(|t| t.split_whitespace().count() >= 1);
    report.meta = Some(meta);

    // `--yes` imports only a born-digital HTML page with a clear title; anything else stops
    // for confirmation, and anything that needs OCR still needs its proofread.
    let apply = opts.yes && format == Format::Html && clear_title;
    if opts.yes && !apply {
        report.stopped = Some(
            "--yes imports only born-digital HTML with a clear title: confirm the draft with \
             wt intake --url, or edit it and run wt text import --apply"
                .into(),
        );
    }
    // The draft's own problems are part of the report, so the person can fix the draft.
    match super::import(
        conn,
        objects,
        embedder,
        &manifest_path,
        opts.allowed_codes,
        opts.actor,
        apply,
        opts.pdf_env,
    )
    .await
    {
        Ok(imported) => report.import = Some(imported),
        Err(e) => report.stopped = Some(format!("the draft did not import: {e}")),
    }
    Ok(report)
}

/// The URLs in a file, one to a line, `#` starting a comment.
pub fn urls_file(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_urls_file_with_comments() {
        assert_eq!(
            urls_file("# list\nhttps://a.example/x\n\n  https://b.example/y  # note\n#https://c\n"),
            vec!["https://a.example/x", "https://b.example/y"]
        );
    }

    #[test]
    fn drafts_a_manifest_that_never_guesses_a_translator() {
        let covering = Covering {
            evidence: sqlx::types::Uuid::nil(),
            kind: "permission".into(),
            license_code: "permission-excerpt".into(),
            publisher: Some("Fixture Press".into()),
            grantor: Some("Fixture Grantor".into()),
            source_key: None,
            prefix: "https://texts.example.invalid/".into(),
        };
        let meta = Meta {
            title: Some("The \"Fixture\" Discourse".into()),
            author: Some("Fixture Author".into()),
            date: Some("1901-05-06T00:00:00Z".into()),
            license_hints: Vec::new(),
        };
        let url = Url::parse("https://texts.example.invalid/discourse").unwrap();
        let text = draft_manifest(&url, &meta, &covering, "source.html", Format::Html);
        let m = super::super::manifest::CorpusManifest::parse(&text).unwrap();
        assert_eq!(m.work.title, "The \"Fixture\" Discourse");
        assert_eq!(m.work.author.as_deref(), Some("Fixture Author"));
        assert_eq!(m.edition.label, "1901-05-06");
        assert_eq!(m.edition.rights_holder, "Fixture Grantor");
        assert!(m.edition.translator.is_none());
        assert_eq!(m.source.format, Format::Html);
    }
}
