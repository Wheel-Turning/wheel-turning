//! `wt intake` (Spec 08, Guided loading): a walkthrough that builds a corpus manifest so nobody
//! writes TOML by hand. It finds folders under `data-inbox/texts/` with no manifest, shows
//! their files, asks for the work and edition, captures the license statement and records
//! approval only when the reviewer types their name, proposes a unit scheme with five sample
//! units, writes `manifest.toml`, shows the dry run, and applies on a yes, moving the folder to
//! `data-inbox/_done/`. With no evidence the folder is marked blocked and nothing proceeds.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use sqlx::types::Uuid;
use wt_core::objects::ObjectStore;
use wt_core::records::LocalRecords;
use wt_ml::Embedder;

use super::extract::{self, Section};
use super::manifest::{Format, Scheme, UnitsSection};
use super::web::{Got, Validators, WebFetcher};
use super::{PdfEnv, TextError, slug, units};

/// The note a blocked folder gets.
const BLOCKED: &str = "BLOCKED.txt";

/// Questions and answers: the terminal, or a script in tests.
pub trait Prompt {
    /// Shows text.
    fn say(&mut self, text: &str);
    /// Asks a question; an empty answer takes the default.
    fn ask(&mut self, question: &str, default: &str) -> Result<String, TextError>;
}

/// A prompt over any reader and writer.
pub struct Terminal<R, W> {
    input: R,
    output: W,
}

impl<R: BufRead, W: Write> Terminal<R, W> {
    /// Reads answers from `input` and writes to `output`.
    pub fn new(input: R, output: W) -> Self {
        Self { input, output }
    }
}

impl<R: BufRead, W: Write> Prompt for Terminal<R, W> {
    fn say(&mut self, text: &str) {
        let _ = writeln!(self.output, "{text}");
    }

    fn ask(&mut self, question: &str, default: &str) -> Result<String, TextError> {
        if default.is_empty() {
            let _ = write!(self.output, "{question}: ");
        } else {
            let _ = write!(self.output, "{question} [{default}]: ");
        }
        let _ = self.output.flush();
        let mut line = String::new();
        let n = self
            .input
            .read_line(&mut line)
            .map_err(|e| TextError::Source(format!("reading an answer: {e}")))?;
        if n == 0 {
            return Err(TextError::Source(
                "the walkthrough ended before it was done".into(),
            ));
        }
        let line = line.trim();
        Ok(if line.is_empty() {
            default.to_owned()
        } else {
            line.to_owned()
        })
    }
}

/// How one folder ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Outcome {
    /// No approved evidence: `BLOCKED.txt` says what is missing.
    Blocked(String),
    /// The manifest is written and the dry run shown; not applied.
    Drafted,
    /// Imported, and the folder moved to `_done/`.
    Applied,
}

/// What intake did with one folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Walked {
    /// The folder.
    pub folder: PathBuf,
    /// How it ended.
    pub outcome: Outcome,
}

/// What intake needs.
pub struct IntakeEnv<'a> {
    /// `[licenses] allowed_license_codes`.
    pub allowed_codes: &'a [String],
    /// Where statements and captured pages are kept.
    pub records: &'a LocalRecords,
    /// For capturing a license page.
    pub fetcher: &'a WebFetcher,
    /// For PDFs.
    pub pdf_env: Option<&'a PdfEnv<'a>>,
}

/// A source file's format from its extension, or none for a file intake does not read.
fn format_of(path: &Path) -> Option<Format> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "epub" => Some(Format::Epub),
        "html" | "htm" | "xhtml" => Some(Format::Html),
        "txt" => Some(Format::Txt),
        "md" => Some(Format::Md),
        "pdf" => Some(Format::Pdf),
        "docx" => Some(Format::Docx),
        _ => None,
    }
}

/// A format as the manifest writes it.
fn format_name(f: Format) -> &'static str {
    match f {
        Format::Epub => "epub",
        Format::Html => "html",
        Format::Txt => "txt",
        Format::Md => "md",
        Format::Pdf => "pdf",
        Format::Docx => "docx",
    }
}

/// Folders under `inbox/texts/` without a usable manifest.
pub fn pending(inbox: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(inbox.join("texts")) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| {
            std::fs::read_to_string(p.join("manifest.toml")).map_or(true, |m| {
                super::manifest::CorpusManifest::parse(&m).is_err()
            })
        })
        .collect();
    out.sort();
    out
}

/// The unit scheme the text suggests: numbered when most blocks start with a number,
/// headings when there are many short titled sections, paragraphs otherwise.
pub fn propose_scheme(sections: &[Section]) -> (Scheme, Option<String>) {
    let blocks: Vec<&String> = sections.iter().flat_map(|s| s.blocks.iter()).collect();
    let numbered = regex::Regex::new(r"^\s*(\d+)[.)]\s").ok();
    let starts = blocks
        .iter()
        .filter(|b| numbered.as_ref().is_some_and(|re| re.is_match(b)))
        .count();
    if !blocks.is_empty() && starts * 2 >= blocks.len() {
        return (Scheme::Numbered, Some(r"^\s*(\d+)[.)]\s".to_owned()));
    }
    let titled = sections.iter().filter(|s| s.title.is_some()).count();
    if titled >= 5 && blocks.len() <= titled * 3 {
        return (Scheme::Heading, None);
    }
    (Scheme::Paragraph, None)
}

/// A unit scheme as the manifest writes it.
fn scheme_name(s: Scheme) -> &'static str {
    match s {
        Scheme::Verse => "verse",
        Scheme::Numbered => "numbered",
        Scheme::Paragraph => "paragraph",
        Scheme::Heading => "heading",
    }
}

/// A TOML string literal on one line, for values written into the manifest.
fn toml_str(s: &str) -> String {
    format!("{:?}", s.replace(['\n', '\r'], " "))
}

/// Captures a license page and asks for the reviewer's name. Returns the evidence when a
/// reviewer approved it.
async fn evidence_by_url(
    conn: &mut sqlx::PgConnection,
    prompt: &mut dyn Prompt,
    env: &IntakeEnv<'_>,
    url: &str,
    publisher: &str,
    code: &str,
    scope: &str,
) -> Result<Option<Uuid>, TextError> {
    let page = url::Url::parse(url).map_err(|e| TextError::Source(format!("{url}: {e}")))?;
    let Got::Body(fetched) = env.fetcher.get(&page, &Validators::default()).await? else {
        return Ok(None);
    };
    let snapshot = env
        .records
        .store(&fetched.bytes)
        .map_err(|e| TextError::Source(e.to_string()))?;
    let id = wt_db::license::capture(
        conn,
        &wt_db::license::Captured {
            publisher,
            code,
            url: &fetched.final_url,
            scope,
            snapshot: &snapshot,
            prefixes: &[],
        },
    )
    .await?;
    let text = extract::from_html(&String::from_utf8_lossy(&fetched.bytes))
        .iter()
        .flat_map(|s| s.blocks.clone())
        .take(12)
        .collect::<Vec<_>>()
        .join("\n");
    prompt.say(&format!("The saved page ({}):\n{text}\n", snapshot.key));
    let reviewer = prompt.ask(
        "Type your name to approve this as the reviewer, or leave it blank to stop",
        "",
    )?;
    if reviewer.trim().is_empty() {
        return Ok(None);
    }
    match wt_db::license::approve(conn, id, reviewer.trim(), env.allowed_codes).await? {
        wt_db::license::Approval::Approved | wt_db::license::Approval::AlreadyApproved => {
            Ok(Some(id))
        }
        other => {
            prompt.say(&format!("Not approved: {other:?}"));
            Ok(None)
        }
    }
}

/// Marks a folder blocked by writing why into it, so the next intake run skips it until the
/// evidence exists.
fn block(folder: &Path, missing: &str) -> Result<Outcome, TextError> {
    let text = format!(
        "This folder is blocked: {missing}\nNothing here is imported until approved evidence \
         exists. Run wt intake again once you have it.\n"
    );
    std::fs::write(folder.join(BLOCKED), text)
        .map_err(|e| TextError::Source(format!("{}: {e}", folder.display())))?;
    Ok(Outcome::Blocked(missing.to_owned()))
}

/// Walks one folder.
#[allow(clippy::too_many_lines)]
pub async fn walk(
    conn: &mut sqlx::PgConnection,
    objects: &dyn ObjectStore,
    embedder: &dyn Embedder,
    prompt: &mut dyn Prompt,
    env: &IntakeEnv<'_>,
    inbox: &Path,
    folder: &Path,
) -> Result<Walked, TextError> {
    let name = folder
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    prompt.say(&format!("\n== {name} =="));
    let mut files: Vec<(PathBuf, Format)> = std::fs::read_dir(folder)
        .map_err(|e| TextError::Source(format!("{}: {e}", folder.display())))?
        .flatten()
        .map(|e| e.path())
        // The walkthrough's own note is not a source.
        .filter(|p| p.file_name().is_none_or(|n| n != BLOCKED))
        .filter_map(|p| format_of(&p).map(|f| (p, f)))
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let Some((first, format)) = files.first().cloned() else {
        return Ok(Walked {
            folder: folder.to_owned(),
            outcome: block(
                folder,
                "no file the importer reads (epub, html, txt, md, pdf, docx)",
            )?,
        });
    };
    let same: Vec<&PathBuf> = files
        .iter()
        .filter(|(_, f)| *f == format)
        .map(|(p, _)| p)
        .collect();
    let mut sections = Vec::new();
    for p in &same {
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let read = if format == Format::Pdf {
            Vec::new()
        } else {
            extract::read(p, format).unwrap_or_default()
        };
        let opening: String = read
            .iter()
            .flat_map(|s| s.blocks.iter())
            .take(2)
            .cloned()
            .collect::<Vec<_>>()
            .join(" / ");
        prompt.say(&format!(
            "  {} ({size} bytes): {}",
            p.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            opening.chars().take(160).collect::<String>()
        ));
        sections.extend(read);
    }

    let role = prompt.ask("Role: corpus or reference", "corpus")?;
    if role.trim() != "corpus" {
        return Ok(Walked {
            folder: folder.to_owned(),
            outcome: block(
                folder,
                "reference assets are written as manifest.toml by hand for now (Spec 08, \
                 reference manifests)",
            )?,
        });
    }
    let stem = first
        .file_stem()
        .map_or_else(|| name.clone(), |s| s.to_string_lossy().into_owned());
    let title = prompt.ask("Work title", &stem)?;
    let author = prompt.ask("Author (blank for none)", "")?;
    let tradition = prompt.ask("Tradition (blank for none)", "")?;
    let label = prompt.ask(
        "Edition label, such as the translator and year",
        "first edition",
    )?;
    let translator = prompt.ask("Translator (blank for none; never guessed)", "")?;
    let language = prompt.ask("Language", "en")?;
    let rights_holder = prompt.ask("Rights holder, or 'public domain'", "public domain")?;

    // The evidence: an approved id, or a page captured now and approved by a typed name.
    let where_ = prompt.ask(
        "Where does the license statement live? A URL, an evidence id, or blank if you have none",
        "",
    )?;
    let evidence: Option<Uuid> = if where_.trim().is_empty() {
        None
    } else if let Ok(id) = where_.trim().parse::<Uuid>() {
        match wt_db::texts::evidence(conn, id).await? {
            Some(ev) if ev.allowed => Some(id),
            Some(_) => {
                prompt.say("That evidence is not approved yet: run wt license approve first.");
                None
            }
            None => None,
        }
    } else {
        let code = prompt.ask("License code", "public-domain")?;
        let scope = if code == "public-domain" {
            prompt.ask(
                "The public-domain basis, such as the translator's death year or the \
                 publisher's statement",
                "",
            )?
        } else {
            prompt.ask("What the license covers", "")?
        };
        let publisher = if rights_holder == "public domain" {
            title.clone()
        } else {
            rights_holder.clone()
        };
        evidence_by_url(conn, prompt, env, where_.trim(), &publisher, &code, &scope).await?
    };
    let Some(evidence) = evidence else {
        let outcome = block(
            folder,
            "no approved license evidence: give the page that states the license or the \
             public-domain basis, and approve it by typing your name as reviewer",
        )?;
        prompt.say("Blocked: nothing proceeds without approved evidence.");
        return Ok(Walked {
            folder: folder.to_owned(),
            outcome,
        });
    };

    // The unit scheme, with five samples.
    let (proposed, pattern) = propose_scheme(&sections);
    let prefix = {
        let s = slug(&title);
        if s.is_empty() {
            "text".to_owned()
        } else {
            s.chars().take(40).collect()
        }
    };
    let mut scheme = proposed;
    loop {
        let cfg = UnitsSection {
            scheme,
            locator_prefix: prefix.clone(),
            number_pattern: pattern.clone().filter(|_| scheme == Scheme::Numbered),
            label_template: "{work}, {n}".into(),
            max_words: 220,
            chapters: false,
        };
        if let Ok(cut) = units::cut(&sections, &cfg, &title) {
            prompt.say(&format!(
                "Proposed scheme: {}. Five sample units:",
                scheme_name(scheme)
            ));
            for u in cut.iter().take(5) {
                prompt.say(&format!(
                    "  {}: {}",
                    u.locator,
                    u.text.chars().take(100).collect::<String>()
                ));
            }
        }
        let answer = prompt.ask(
            "Keep this scheme, or type verse, numbered, paragraph or heading",
            scheme_name(scheme),
        )?;
        let chosen = match answer.trim() {
            "verse" => Scheme::Verse,
            "numbered" if pattern.is_some() => Scheme::Numbered,
            "paragraph" => Scheme::Paragraph,
            "heading" => Scheme::Heading,
            _ => scheme,
        };
        if chosen == scheme {
            break;
        }
        scheme = chosen;
    }

    let mut m = String::from("role = \"corpus\"\n\n[work]\n");
    m.push_str(&format!("title = {}\n", toml_str(&title)));
    if !author.trim().is_empty() {
        m.push_str(&format!("author = {}\n", toml_str(author.trim())));
    }
    if !tradition.trim().is_empty() {
        m.push_str(&format!("tradition = {}\n", toml_str(tradition.trim())));
    }
    m.push_str(&format!("\n[edition]\nlabel = {}\n", toml_str(&label)));
    if !translator.trim().is_empty() {
        m.push_str(&format!("translator = {}\n", toml_str(translator.trim())));
    }
    m.push_str(&format!(
        "language = {}\nrights_holder = {}\nlicense_evidence_id = \"{evidence}\"\n",
        toml_str(&language),
        toml_str(&rights_holder)
    ));
    let file_list: Vec<String> = same
        .iter()
        .filter_map(|p| p.file_name().map(|n| toml_str(&n.to_string_lossy())))
        .collect();
    m.push_str(&format!(
        "\n[source]\nfiles = [{}]\nformat = \"{}\"\n",
        file_list.join(", "),
        format_name(format)
    ));
    m.push_str(&format!(
        "\n[units]\nscheme = \"{}\"\nlocator_prefix = {}\nlabel_template = \"{{work}}, {{n}}\"\n",
        scheme_name(scheme),
        toml_str(&prefix)
    ));
    if scheme == Scheme::Numbered
        && let Some(p) = &pattern
    {
        m.push_str(&format!("number_pattern = '{p}'\n"));
    }
    let manifest_path = folder.join("manifest.toml");
    std::fs::write(&manifest_path, &m)
        .map_err(|e| TextError::Source(format!("{}: {e}", manifest_path.display())))?;
    let _ = std::fs::remove_file(folder.join(BLOCKED));

    let dry = super::import(
        conn,
        objects,
        embedder,
        &manifest_path,
        &super::ImportOptions {
            allowed_codes: env.allowed_codes,
            actor: "intake",
            apply: false,
            force: false,
            pdf_env: env.pdf_env,
        },
    )
    .await?;
    prompt.say(&format!(
        "Dry run: {} units in {} sections kept ({} excluded); words per unit {:?}; {} warnings{}",
        dry.units,
        dry.sections_kept.len(),
        dry.sections_excluded.len(),
        dry.words,
        dry.warnings.len(),
        dry.notice_outcome
            .as_deref()
            .map(|o| format!("; notice: {o}"))
            .unwrap_or_default()
    ));
    if dry.skipped.is_some()
        || dry
            .notice_outcome
            .as_deref()
            .is_some_and(|o| o.starts_with("blocked"))
    {
        return Ok(Walked {
            folder: folder.to_owned(),
            outcome: Outcome::Drafted,
        });
    }
    let apply = prompt.ask("Apply? yes or no", "no")?;
    if !matches!(apply.trim(), "yes" | "y") {
        return Ok(Walked {
            folder: folder.to_owned(),
            outcome: Outcome::Drafted,
        });
    }
    let applied = super::import(
        conn,
        objects,
        embedder,
        &manifest_path,
        &super::ImportOptions {
            allowed_codes: env.allowed_codes,
            actor: "intake",
            apply: true,
            force: false,
            pdf_env: env.pdf_env,
        },
    )
    .await?;
    if !applied.applied && !applied.unchanged {
        prompt.say("The import did not apply; the folder stays.");
        return Ok(Walked {
            folder: folder.to_owned(),
            outcome: Outcome::Drafted,
        });
    }
    // The originals are stored, so the folder moves to _done/ and can be deleted.
    let done = inbox.join("_done");
    std::fs::create_dir_all(&done)
        .map_err(|e| TextError::Source(format!("{}: {e}", done.display())))?;
    let to = done.join(&name);
    std::fs::rename(folder, &to)
        .map_err(|e| TextError::Source(format!("moving to {}: {e}", to.display())))?;
    prompt.say(&format!("Applied; moved to {}", to.display()));
    Ok(Walked {
        folder: to,
        outcome: Outcome::Applied,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(blocks: &[&str]) -> Section {
        Section {
            title: None,
            blocks: blocks.iter().map(|b| (*b).to_owned()).collect(),
        }
    }

    #[test]
    fn proposes_numbered_or_paragraph() {
        let (s, p) = propose_scheme(&[section(&[
            "1. One verse here.",
            "2. Two verse here.",
            "Note.",
        ])]);
        assert_eq!(s, Scheme::Numbered);
        assert!(p.is_some());
        let (s, _) = propose_scheme(&[section(&["Plain prose here.", "More prose."])]);
        assert_eq!(s, Scheme::Paragraph);
    }
}
