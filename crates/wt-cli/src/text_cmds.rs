//! `wt text`, `wt license` and `wt reference`: foundational texts, their license evidence
//! and reference assets (Spec 08). The work is in `index_cmds`.

use std::path::PathBuf;

use anyhow::Context;
use clap::Subcommand;

use crate::{Ctx, commands, index_cmds, stages};

/// `wt text`'s subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum TextCommand {
    /// Records a work read elsewhere, with its locators and no text, so talks can cite it and
    /// the footnote links out.
    LinkOut {
        /// The work's title.
        #[arg(long)]
        work: String,
        /// Where to read it.
        #[arg(long)]
        url: String,
        /// A CSV of locators: locator, label, and optional aliases (semicolon separated) and url.
        #[arg(long)]
        locators: Option<PathBuf>,
        /// Another name talks use for the whole work, beside its title; repeat for more.
        #[arg(long = "alias")]
        aliases: Vec<String>,
        /// The directory holding pipeline.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// Imports a corpus text from a manifest and its source files: a dry run that reports
    /// sections, units and locators unless --apply, which writes the edition, its units with
    /// embeddings and its locators. An unchanged source and manifest make --apply a no-op.
    Import {
        /// The manifest TOML, beside the source files.
        #[arg(long)]
        manifest: PathBuf,
        /// Write; without it, only report. A large PDF is queued as ocr_chunk jobs.
        #[arg(long, conflicts_with = "estimate")]
        apply: bool,
        /// For a PDF: measure pages, OCR needs and speed, and queue nothing.
        #[arg(long)]
        estimate: bool,
        /// The worker concurrency the estimate assumes.
        #[arg(long, default_value_t = 1)]
        concurrency: usize,
        /// Who is importing.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// The directory holding pipeline.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// Writes a standalone proof sheet for an edition: each sampled unit beside a crop of its
    /// page, to mark and save as proof.json.
    Proof {
        /// The edition.
        #[arg(long)]
        edition: sqlx::types::Uuid,
        /// Where the sheet goes.
        #[arg(long, default_value = "data-inbox/_proof")]
        out_dir: PathBuf,
    },
    /// Records a proofread from the sheet's proof.json. A pass makes the edition ready.
    ProofImport {
        /// The edition.
        #[arg(long)]
        edition: sqlx::types::Uuid,
        /// The saved proof.json.
        #[arg(long)]
        file: PathBuf,
        /// Who read the units.
        #[arg(long)]
        reviewer: String,
        /// The directory holding pipeline.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// Gives every text unit embedded with another model or an older vector version a new
    /// vector from its stored text, without importing the edition again (Spec 04). Prints the
    /// count as JSON; a rerun with nothing stale is a no-op.
    Reembed,
    /// Reads a page range of a PDF edition again at a higher resolution and imports the
    /// edition again; it then needs a new proofread.
    Reocr {
        /// The edition.
        #[arg(long)]
        edition: sqlx::types::Uuid,
        /// The pages, such as `120-180`.
        #[arg(long)]
        pages: String,
        /// The resolution.
        #[arg(long, default_value_t = 400)]
        dpi: u32,
        /// Who is doing it.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// The directory holding pipeline.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// Fetches documents by URL through the evidence gate and drafts their manifests in
    /// data-inbox/texts/. A URL no approved evidence or enabled source covers gets a HEAD
    /// request only.
    Fetch {
        /// A URL; repeat for more.
        #[arg(long)]
        url: Vec<String>,
        /// A file of URLs, one to a line, `#` for comments.
        #[arg(long)]
        urls_file: Option<PathBuf>,
        /// An index page to crawl instead: its links are listed for a person to choose from,
        /// and nothing is imported.
        #[arg(long, conflicts_with_all = ["url", "urls_file", "yes"])]
        crawl: Option<String>,
        /// Link levels below the index to read.
        #[arg(long, default_value_t = 1)]
        depth: u32,
        /// A regular expression for links that are documents.
        #[arg(long)]
        follow_pattern: Option<String>,
        /// The most documents listed.
        #[arg(long, default_value_t = 50)]
        max_pages: usize,
        /// Import a born-digital HTML page with a clear title without asking.
        #[arg(long)]
        yes: bool,
        /// Where drafts go.
        #[arg(long, default_value = "data-inbox")]
        inbox: PathBuf,
        /// Who is fetching.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// The directory holding pipeline.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// Refetches an edition's source with a conditional request. Unchanged is a no-op; a
    /// changed document is compared unit by unit, and --apply imports it, which then needs its
    /// proofread.
    Refresh {
        /// The edition.
        #[arg(long)]
        edition: sqlx::types::Uuid,
        /// Import the new version.
        #[arg(long)]
        apply: bool,
        /// Who.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// The directory holding pipeline.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// Stops an edition serving at once, keeping everything, until released.
    Quarantine {
        /// The edition.
        #[arg(long)]
        edition: sqlx::types::Uuid,
        /// Why.
        #[arg(long)]
        reason: String,
        /// Who.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Releases a quarantined edition: ready again if its proofread still holds.
    Release {
        /// The edition.
        #[arg(long)]
        edition: sqlx::types::Uuid,
        /// Who.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Corrects one unit's text. Its edition then needs a new proofread before it serves.
    Fix {
        /// The unit.
        #[arg(long)]
        unit: sqlx::types::Uuid,
        /// The corrected text.
        #[arg(long)]
        text: String,
        /// Who is correcting it.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// Why.
        #[arg(long, default_value = "proofread correction")]
        reason: String,
    },
}

/// `wt license`'s subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum LicenseCommand {
    /// Saves a license page and records it as evidence, unapproved until a person reads it.
    Capture {
        /// Who publishes what it covers.
        #[arg(long)]
        publisher: String,
        /// The page stating the license.
        #[arg(long)]
        url: String,
        /// Such as cc-by-4.0 or public-domain.
        #[arg(long)]
        code: String,
        /// What it covers, in words.
        #[arg(long)]
        scope: String,
        /// URL prefixes it covers, for `wt text fetch`.
        #[arg(long)]
        prefix: Vec<String>,
        /// The directory holding pipeline.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// Records an organization's permission for URL prefixes, unapproved until a person reads
    /// the statement. A permission covers only what the grantor can authorize.
    RecordPermission {
        /// The organization.
        #[arg(long)]
        grantor: String,
        /// URL prefixes it covers.
        #[arg(long, required = true)]
        scope_urls: Vec<String>,
        /// What it allows.
        #[arg(long)]
        covers: String,
        /// The statement, such as an email pasted into a text file.
        #[arg(long)]
        statement_file: PathBuf,
        /// When it was given, YYYY-MM-DD.
        #[arg(long)]
        given_on: chrono::NaiveDate,
        /// Who is recording it.
        #[arg(long)]
        recorded_by: String,
        /// The directory holding pipeline.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// A person's approval after reading the snapshot; refused for a code not in
    /// allowed_license_codes.
    Approve {
        /// The evidence.
        #[arg(long)]
        id: sqlx::types::Uuid,
        /// Who read it.
        #[arg(long)]
        reviewer: String,
        /// The directory holding pipeline.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
}

/// `wt reference`'s subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum ReferenceCommand {
    /// Imports a reference asset from a manifest and its CSV files: a dry run unless --apply.
    /// A glossary change queues correction for the talks it affects.
    Import {
        /// The manifest TOML, beside the files.
        #[arg(long)]
        manifest: PathBuf,
        /// Write; without it, only report.
        #[arg(long)]
        apply: bool,
        /// For a lineage file: link teachers whose display names match a person exactly.
        #[arg(long)]
        link_teachers: bool,
        /// Who is importing.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// The directory holding pipeline.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
}

/// `wt text`: imports, proofs, fetches and takes down foundational texts (Spec 08).
pub(crate) async fn text(ctx: &Ctx, command: TextCommand) -> anyhow::Result<()> {
    match command {
        TextCommand::Import {
            manifest,
            estimate: true,
            concurrency,
            ..
        } => {
            let tools = wt_ingest::texts::pdf::tools::Poppler::from_config(&ctx.config);
            let estimate = wt_ingest::texts::large::estimate(
                &tools,
                &manifest,
                &ctx.config.scratch_dir.join("texts"),
                concurrency,
            )?;
            commands::print_json(&estimate)?;
        }
        TextCommand::Import {
            manifest,
            apply,
            by,
            config_dir,
            ..
        } => {
            let report = index_cmds::import_text(
                &ctx.pool().await?,
                &ctx.config,
                &config_dir,
                &manifest,
                apply,
                &by,
            )
            .await?;
            commands::print_json(&report)?;
        }
        TextCommand::Proof { edition, out_dir } => {
            let path =
                index_cmds::proof_sheet(&ctx.pool().await?, &ctx.config, edition, &out_dir).await?;
            commands::print_json(&serde_json::json!({ "sheet": path }))?;
        }
        TextCommand::ProofImport {
            edition,
            file,
            reviewer,
            config_dir,
        } => {
            let report = index_cmds::proof_import(
                &ctx.pool().await?,
                &ctx.config,
                edition,
                &file,
                &reviewer,
                &config_dir,
            )
            .await?;
            commands::print_json(&report)?;
        }
        TextCommand::Reembed => {
            let report = index_cmds::reembed_texts(&ctx.pool().await?, &ctx.config).await?;
            commands::print_json(&report)?;
        }
        TextCommand::Reocr {
            edition,
            pages,
            dpi,
            by,
            config_dir,
        } => {
            let report = index_cmds::reocr(
                &ctx.pool().await?,
                &ctx.config,
                &config_dir,
                edition,
                &pages,
                dpi,
                &by,
            )
            .await?;
            commands::print_json(&report)?;
        }
        TextCommand::Fetch {
            crawl: Some(start),
            depth,
            follow_pattern,
            max_pages,
            inbox,
            config_dir,
            ..
        } => {
            let report = index_cmds::crawl_texts(
                &ctx.pool().await?,
                &config_dir,
                &start,
                depth,
                follow_pattern.as_deref(),
                max_pages,
                &inbox,
            )
            .await?;
            commands::print_json(&report)?;
        }
        TextCommand::Fetch {
            url,
            urls_file,
            yes,
            inbox,
            by,
            config_dir,
            ..
        } => {
            let mut urls = url;
            if let Some(file) = urls_file {
                let text = std::fs::read_to_string(&file)
                    .with_context(|| format!("reading {}", file.display()))?;
                urls.extend(wt_ingest::texts::url::urls_file(&text));
            }
            if urls.is_empty() {
                anyhow::bail!("give --url or --urls-file");
            }
            let reports = index_cmds::fetch_texts(
                &ctx.pool().await?,
                &ctx.config,
                &config_dir,
                &urls,
                yes,
                &inbox,
                &by,
            )
            .await?;
            commands::print_json(&reports)?;
        }
        TextCommand::Refresh {
            edition,
            apply,
            by,
            config_dir,
        } => {
            let report = index_cmds::refresh_text(
                &ctx.pool().await?,
                &ctx.config,
                &config_dir,
                edition,
                apply,
                &by,
            )
            .await?;
            commands::print_json(&report)?;
        }
        TextCommand::Quarantine {
            edition,
            reason,
            by,
        } => {
            let pool = ctx.pool().await?;
            let mut conn = pool.acquire().await?;
            let was =
                wt_ingest::texts::takedown::quarantine(&mut conn, edition, &reason, &by).await?;
            commands::print_json(&serde_json::json!({
                "edition": edition, "was": was, "state": "quarantined"
            }))?;
        }
        TextCommand::Release { edition, by } => {
            let pool = ctx.pool().await?;
            let mut conn = pool.acquire().await?;
            let state = wt_ingest::texts::takedown::release(&mut conn, edition, &by).await?;
            commands::print_json(&serde_json::json!({ "edition": edition, "state": state }))?;
        }
        TextCommand::Fix {
            unit,
            text,
            by,
            reason,
        } => {
            let pool = ctx.pool().await?;
            let mut conn = pool.acquire().await?;
            let embedder = stages::embedder(&ctx.config)?;
            let report = wt_ingest::texts::proofread::fix_unit(
                &mut conn, &embedder, unit, &text, &by, &reason,
            )
            .await?;
            commands::print_json(&report)?;
        }
        TextCommand::LinkOut {
            work,
            url,
            locators,
            aliases,
            config_dir,
        } => {
            let stoplist =
                wt_ingest::config::CitationSettings::load(&config_dir.join("pipeline.toml"))?
                    .work_alias_stoplist;
            let warnings: Vec<String> = std::iter::once(&work)
                .chain(&aliases)
                .filter_map(|n| wt_extract::resolve::work_name_warning(n, &stoplist))
                .collect();
            let pool = ctx.pool().await?;
            let mut conn = pool.acquire().await?;
            let n = wt_ingest::texts::reference::link_out(
                &mut conn,
                &work,
                &url,
                locators.as_deref(),
                &aliases,
            )
            .await?;
            commands::print_json(
                &serde_json::json!({ "work": work, "locators": n, "warnings": warnings }),
            )?;
        }
    }
    Ok(())
}

/// `wt reference`: imports reference assets (Spec 08).
pub(crate) async fn reference(ctx: &Ctx, command: ReferenceCommand) -> anyhow::Result<()> {
    match command {
        ReferenceCommand::Import {
            manifest,
            apply,
            link_teachers,
            by,
            config_dir,
        } => {
            let report = index_cmds::import_reference(
                &ctx.pool().await?,
                &ctx.config,
                &config_dir,
                &manifest,
                apply,
                link_teachers,
                &by,
            )
            .await?;
            commands::print_json(&report)?;
        }
    }
    Ok(())
}
