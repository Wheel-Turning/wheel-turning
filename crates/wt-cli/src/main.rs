//! The wt binary: commands and the worker.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use wt_core::Config;
use wt_core::gate::ConsentStatus;
use wt_core::ids::TalkId;
use wt_core::telemetry::{LogOutput, init_logging};

mod asr_cmds;
mod commands;
mod index_cmds;
mod ingest;
mod rights;
mod source;
mod stages;

#[derive(Debug, Parser)]
#[command(name = "wt", version, about = "Wheel Turning operations")]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Where statements, drafts and mandate decisions are stored, by SHA-256.
    #[arg(long, global = true, default_value = "data/records")]
    records_dir: PathBuf,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Database operations.
    Db {
        #[command(subcommand)]
        command: DbCommand,
    },
    /// The teacher, alias and sangha registry (Spec 03).
    Registry {
        #[command(subcommand)]
        command: RegistryCommand,
    },
    /// Speaker consent (Spec 03, the consent gate).
    Consent {
        #[command(subcommand)]
        command: ConsentCommand,
    },
    /// Rights holders and hand exclusions for talks (Spec 03, rule 5).
    Talk {
        #[command(subcommand)]
        command: TalkCommand,
    },
    /// Remove a teacher's or a talk's text from every store (Spec 07). Prints the counts as
    /// JSON. A real run needs --yes.
    Takedown(TakedownArgs),
    /// Run the integrity checks (Spec 07). Prints a JSON report and exits 1 when a check marked
    /// fail has findings.
    Audit,
    /// Discovery from the configured sources (Spec 03).
    Ingest {
        #[command(subcommand)]
        command: IngestCommand,
    },
    /// Run queued jobs (Spec 03, Job queue).
    Worker(WorkerArgs),
    /// Sources and their settings (Spec 03, Seattle Soto Zen sources; Spec 08, Mandated
    /// sources).
    Source {
        #[command(subcommand)]
        command: SourceCommand,
    },
    /// The rights ledger: permission requests and their answers (Spec 08, Rights lifecycle).
    Rights {
        #[command(subcommand)]
        command: RightsCommand,
    },
    /// Permanent exclusions (Spec 08, Rights lifecycle).
    Exclusion {
        #[command(subcommand)]
        command: ExclusionCommand,
    },
    /// Speech recognition: the benchmark and model changes (Spec 04).
    Asr {
        #[command(subcommand)]
        command: AsrCommand,
    },
    /// Moves cleared talks into the pipeline and retries failed stages (Spec 03).
    Pipeline {
        #[command(subcommand)]
        command: PipelineCommand,
    },
    /// Foundational texts (Spec 08).
    Text {
        #[command(subcommand)]
        command: TextCommand,
    },
    /// Reference assets: glossaries, alias lists, lineage and locator lists (Spec 08).
    Reference {
        #[command(subcommand)]
        command: ReferenceCommand,
    },
    /// Offline transcript review (Spec 04, Transcript review and owner exports).
    Transcript {
        #[command(subcommand)]
        command: TranscriptCommand,
    },
    /// Reruns the index step from stored segments, with a new glossary or embedding model
    /// (Spec 04).
    Reindex(ReindexArgs),
    /// Exports for the evaluation notebooks (Spec 07).
    Export {
        #[command(subcommand)]
        command: ExportCommand,
    },
    /// Segmentation tools (Spec 04).
    Segment {
        #[command(subcommand)]
        command: SegmentCommand,
    },
    /// The glossary (Spec 04, Glossary correction).
    Glossary {
        #[command(subcommand)]
        command: GlossaryCommand,
    },
    /// The language-model spend cap (Spec 04).
    Llm {
        #[command(subcommand)]
        command: LlmCommand,
    },
    /// A teacher's standing (Spec 03, Mandated talks).
    Teacher {
        #[command(subcommand)]
        command: TeacherCommand,
    },
}

#[derive(Debug, clap::Args)]
struct BenchmarkArgs {
    /// A clip with known text: a public-domain or CC0 recording, never a teacher's talk.
    #[arg(long, conflicts_with = "talk")]
    clip: Option<PathBuf>,
    /// The clip's known text.
    #[arg(long, requires = "clip")]
    reference: Option<PathBuf>,
    /// Cleared talks to fetch instead. Repeat for several.
    #[arg(long)]
    talk: Vec<TalkId>,
    /// Hand-corrected text for talks, as <dir>/<talk id>.txt. Keep it under data/.
    #[arg(long, requires = "talk")]
    references: Option<PathBuf>,
    /// The setups and models.
    #[arg(long, default_value = "config/asr_bench.toml")]
    setups: PathBuf,
    /// Where the file's relative paths start.
    #[arg(long, default_value = ".")]
    bench_root: PathBuf,
    /// Only this setup. Repeat for several.
    #[arg(long = "setup")]
    only_setups: Vec<String>,
    /// Only this model. Repeat for several.
    #[arg(long = "model")]
    only_models: Vec<String>,
    /// Append the result, the date and the hardware to this file.
    #[arg(long, num_args = 0..=1, default_missing_value = "docs/asr-benchmark.md")]
    record: Option<PathBuf>,
    /// A line for the record, such as the GPU and its driver.
    #[arg(long)]
    note: Option<String>,
    /// Run without the glossary prompt, whatever [asr] glossary_prompt says.
    #[arg(long, conflicts_with = "prompt")]
    no_prompt: bool,
    /// Run with the glossary prompt, whatever [asr] glossary_prompt says.
    #[arg(long)]
    prompt: bool,
    /// The directory holding pipeline.toml.
    #[arg(long, default_value = "config")]
    config_dir: PathBuf,
}

#[derive(Debug, Subcommand)]
enum AsrCommand {
    /// Runs every available setup and model on a clip, or on talks, and prints the real-time
    /// factor, peak memory, word error rate and error rates on glossary terms and teacher names,
    /// then the setup Spec 04's decision rule picks.
    Benchmark(Box<BenchmarkArgs>),
    /// Queues retranscription for every talk transcribed with a model other than
    /// WT_WHISPER_MODEL's.
    Requeue {
        /// Count them without queueing anything.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ReferenceCommand {
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

#[derive(Debug, Subcommand)]
enum TextCommand {
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

#[derive(Debug, Subcommand)]
enum TranscriptCommand {
    /// Writes one review file per talk: an HTML page with the corrected text paragraph by
    /// paragraph, each editable, and a "Listen at" link to that moment of the recording. Saving
    /// in the page downloads the corrections as JSON.
    ExportReview {
        /// One talk.
        #[arg(long, conflicts_with = "teacher", required_unless_present = "teacher")]
        talk: Option<TalkId>,
        /// Every transcribed talk of this teacher.
        #[arg(long)]
        teacher: Option<String>,
        /// The directory to write into. Keep it under data/: the files hold teachers' words.
        #[arg(long, default_value = "data/review")]
        out: PathBuf,
        /// The directory holding pipeline.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// Applies a corrections file as a reviewed transcript version, which no automated pass
    /// replaces, and queues the talk to be indexed again.
    ImportCorrections {
        /// The JSON the review page saved.
        #[arg(long)]
        file: PathBuf,
        /// Who reviewed it.
        #[arg(long)]
        reviewer: String,
    },
}

#[derive(Debug, Subcommand)]
enum PipelineCommand {
    /// Queues an audio job for every talk at cleared.
    Advance,
    /// Requeues a talk's dead job with fresh attempts.
    Retry {
        /// The talk.
        #[arg(long)]
        talk: TalkId,
    },
}

#[derive(Debug, clap::Args)]
#[command(group(clap::ArgGroup::new("which").required(true).args(["talk", "all", "citations"])))]
struct ReindexArgs {
    /// A talk to reindex. Repeat for several.
    #[arg(long)]
    talk: Vec<TalkId>,
    /// Every talk with corrected segments.
    #[arg(long)]
    all: bool,
    /// Relink citations to text units after a text or locator import, and queue resolution
    /// again for talks with staged citations when the hand list changed (Spec 08).
    #[arg(long)]
    citations: bool,
    /// The embedding model directory under WT_MODEL_DIR, instead of WT_EMBED_MODEL.
    #[arg(long)]
    model: Option<String>,
    /// The directory holding pipeline.toml.
    #[arg(long, default_value = "config")]
    config_dir: PathBuf,
}

#[derive(Debug, Subcommand)]
enum ExportCommand {
    /// Writes a teacher's talks for their own use, such as books (Spec 04, Owner exports): one
    /// Markdown file per talk with the teacher's words in paragraphs and the removed stretches
    /// marked, whatever the talk's display level. No student speech, no audio.
    Teacher {
        /// Display name or alias.
        #[arg(long)]
        teacher: String,
        /// The directory to write into. Keep it under data/, which git ignores.
        #[arg(long, default_value = "data/owner-export")]
        out: PathBuf,
        /// The directory holding pipeline.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// Writes passages.jsonl: one line per servable passage with its id, opaque talk and
    /// teacher ids, start time and text. No names, titles or audio.
    Transcripts {
        /// Output format.
        #[arg(long, value_parser = ["jsonl"], default_value = "jsonl")]
        format: String,
        /// The directory to write into. Keep it under data/, which git ignores.
        #[arg(long, default_value = "data/export")]
        out: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum SegmentCommand {
    /// Runs fetch, recognition, diarization and segmentation on cleared talks without storing
    /// anything, and writes an HTML timeline per talk.
    Sample {
        /// How many cleared talks to pick at random.
        #[arg(long, default_value_t = 10)]
        count: i64,
        /// Specific talks instead of a random pick. Repeat for several.
        #[arg(long)]
        talk: Vec<TalkId>,
        /// The report file. Keep it under data/: it holds teachers' words.
        #[arg(long, default_value = "data/segment-sample.html")]
        out: PathBuf,
        /// The directory holding pipeline.toml and series.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum GlossaryCommand {
    /// Load terms from a glossary file, such as scripts/seed_glossary.toml. Variants merge into
    /// existing terms; a conflicting spelling or kind is reported and left as stored. Then
    /// queues correction for exactly the talks the change affects.
    Import {
        /// The glossary TOML.
        #[arg(long)]
        file: PathBuf,
        /// Who is making the change.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Add one term, or variants to an existing term, then queue correction for the talks it
    /// affects.
    Add {
        /// The canonical spelling.
        #[arg(long)]
        canonical: String,
        /// term, name or text_title.
        #[arg(long, value_parser = ["term", "name", "text_title"], default_value = "term")]
        kind: String,
        /// A known variant. Repeat for several.
        #[arg(long = "variant")]
        variants: Vec<String>,
        /// Who is making the change.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
}

#[derive(Debug, Subcommand)]
enum LlmCommand {
    /// Inspect or change the monthly cap.
    Cap {
        #[command(subcommand)]
        command: CapCommand,
    },
}

#[derive(Debug, Subcommand)]
enum CapCommand {
    /// Set the monthly cap in US dollars, and release ingestion jobs the cap had put off.
    Set {
        /// The cap.
        usd: f64,
        /// Who is changing it.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Print the cap and the spend so far this month.
    Show,
}

#[derive(Debug, Subcommand)]
enum TeacherCommand {
    /// Take a teacher out. Their passages leave the serving view at once and no job of theirs
    /// passes the gate. Unless --suspend-only, it then deletes their passages, embeddings,
    /// segments and transcripts and excludes the person and the transcripts' content hashes
    /// for good, which needs --yes.
    OptOut {
        /// Display name or alias.
        #[arg(long)]
        teacher: String,
        /// Why, in their words or yours.
        #[arg(long)]
        reason: String,
        /// Who is recording it.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// Only suspend, while a request is verified. Deletes nothing.
        #[arg(long, conflicts_with = "yes")]
        suspend_only: bool,
        /// Confirm the takedown and exclusions.
        #[arg(long)]
        yes: bool,
    },
    /// Set how much of a teacher's talks is shown (Spec 03, Display levels): a excluded,
    /// b excerpts only, c full (the default), d owner only, e export then delete, f full
    /// removal. Narrowing applies at once; widening needs --evidence pointing to the teacher's
    /// own request. e writes the owner export, then runs the opt-out takedown; f runs the
    /// opt-out takedown. Both need --yes.
    Display {
        /// Display name or alias.
        #[arg(long)]
        teacher: String,
        /// a, b, c or d, or excluded, excerpts, full, owner_only.
        #[arg(long, value_parser = parse_display_level)]
        level: wt_db::display::DisplayLevel,
        /// Only this talk: a, b or d, no wider than the teacher's level.
        #[arg(long)]
        talk: Option<TalkId>,
        /// Why, in their words or yours.
        #[arg(long)]
        reason: String,
        /// Who is recording it.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// Pointer to the teacher's own request, needed to widen a level.
        #[arg(long)]
        evidence: Option<String>,
        /// For e: where the owner export goes before the takedown.
        #[arg(long, default_value = "data/owner-export")]
        out: PathBuf,
        /// For e and f: confirm the takedown and exclusions.
        #[arg(long)]
        yes: bool,
        /// The directory holding pipeline.toml, for e's export.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
}

fn parse_display_level(value: &str) -> Result<wt_db::display::DisplayLevel, String> {
    wt_db::display::DisplayLevel::parse(value)
        .ok_or_else(|| format!("unknown display level {value:?}; use a to f or its name"))
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct SourceOrTeacher {
    /// A source, by key.
    #[arg(long)]
    source: Option<String>,
    /// A teacher's notice, by display name or alias.
    #[arg(long)]
    teacher: Option<String>,
}

#[derive(Debug, Subcommand)]
enum RightsCommand {
    /// Record a drafted permission request. Nothing is sent: a person sends it by hand.
    Request {
        /// The source key.
        #[arg(long)]
        source: String,
        /// The address or contact page it goes to.
        #[arg(long)]
        to: String,
        /// email, form or other.
        #[arg(long, value_parser = ["email", "form", "other"])]
        channel: String,
        /// The drafted request, stored as a record.
        #[arg(long)]
        draft_file: PathBuf,
        /// Who is recording it.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Record that the latest drafted request or teacher notice went out, and schedule its
    /// follow-up 14 days later.
    Sent {
        #[command(flatten)]
        to: SourceOrTeacher,
        /// The day it went out, YYYY-MM-DD.
        #[arg(long)]
        on: chrono::NaiveDate,
        /// Who is recording it.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Record a grant: stores the reply as permission evidence for the source's prefixes and
    /// switches its basis to permission.
    Grant {
        /// The source key.
        #[arg(long)]
        source: String,
        /// The reply, for example an email pasted into a text file.
        #[arg(long)]
        statement_file: PathBuf,
        /// Who gave it.
        #[arg(long)]
        from: String,
        /// The day it was given, YYYY-MM-DD.
        #[arg(long)]
        given_on: chrono::NaiveDate,
        /// What it covers, in the grantor's words.
        #[arg(long)]
        scope: String,
        /// Who is recording it.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Record a refusal: switches the source off and excludes its prefixes permanently. Needs
    /// --yes.
    Decline {
        /// The source key.
        #[arg(long)]
        source: String,
        /// The reply, stored as a record.
        #[arg(long)]
        statement_file: PathBuf,
        /// Who is recording it.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// Confirm.
        #[arg(long)]
        yes: bool,
    },
    /// Record that the latest sent request or notice went unanswered. Silence is never
    /// permission: a source stays on its mandate.
    NoResponse {
        #[command(flatten)]
        to: SourceOrTeacher,
        /// Who is recording it.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Print the ledger in order: everything, one source's, or one teacher's.
    Ledger {
        /// Only this source.
        #[arg(long, conflicts_with = "teacher")]
        source: Option<String>,
        /// Only this teacher, by display name or alias.
        #[arg(long)]
        teacher: Option<String>,
    },
    /// Turn mandated talks on (mandate) or off (consent). Off deletes nothing: the serving
    /// view drops mandated talks and their jobs stop at the gate. On clears waiting talks that
    /// now pass.
    SetTalksBasis {
        /// mandate or consent.
        #[arg(value_parser = ["mandate", "consent"])]
        basis: String,
        /// Who decided.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Draft the notice to a teacher from templates/teacher_notice.md and record it as a
    /// drafted request. Nothing is sent: a person sends it by hand, then runs `rights sent
    /// --teacher`.
    NoticeDraft {
        /// Display name or alias.
        #[arg(long)]
        teacher: String,
        /// Where it will go: an address or a contact page.
        #[arg(long)]
        to: String,
        /// email, form or other.
        #[arg(long, value_parser = ["email", "form", "other"], default_value = "email")]
        channel: String,
        /// Where to write the drafted notice.
        #[arg(long)]
        out: PathBuf,
        /// The sender's name, for the signature.
        #[arg(long)]
        from_name: Option<String>,
        /// How to reach the sender, for the signature.
        #[arg(long)]
        contact: Option<String>,
        /// The template.
        #[arg(long, default_value = "templates/teacher_notice.md")]
        template: PathBuf,
        /// Who is recording it.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// List requests not yet sent and requests past their follow-up date.
    Due {
        /// The day to check against, YYYY-MM-DD. Defaults to today (UTC).
        #[arg(long)]
        today: Option<chrono::NaiveDate>,
    },
}

#[derive(Debug, Subcommand)]
enum ExclusionCommand {
    /// Every active exclusion.
    List,
    /// Lift an exclusion. Needs the rights holder's own written grant, which is stored and
    /// cited.
    Lift {
        /// The exclusion id.
        #[arg(long)]
        id: sqlx::types::Uuid,
        /// Who is lifting it.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// The rights holder's written grant.
        #[arg(long)]
        evidence: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum SourceCommand {
    /// Add a source of texts or talks. By default it is used on the owner's mandate, recorded
    /// with a rationale and the attribution line shown on excerpts; --basis license or
    /// permission uses approved evidence instead. Refused if an exclusion covers a prefix.
    #[command(visible_alias = "mandate")]
    Add {
        /// Short key, such as bdkamerica.
        #[arg(long)]
        key: String,
        /// The publisher or rights holder.
        #[arg(long)]
        publisher: String,
        /// A URL prefix the source covers. Repeat for several.
        #[arg(long = "prefix", required = true)]
        prefixes: Vec<String>,
        /// mandate, license or permission. Defaults to [rights] default_basis_for_new_sources
        /// in pipeline.toml.
        #[arg(long, value_parser = parse_basis)]
        basis: Option<wt_db::rights::Basis>,
        /// The directory holding pipeline.toml, read when --basis is not given.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
        /// texts or talks. A talks source is named by `rights_source` in sources.toml.
        #[arg(long, default_value = "texts", value_parser = ["texts", "talks"])]
        kind: String,
        /// The approved license or permission evidence, for those bases.
        #[arg(long)]
        evidence: Option<sqlx::types::Uuid>,
        /// Who decided.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// Why the owner relies on the source. Required for a mandate.
        #[arg(long)]
        rationale: Option<String>,
        /// The attribution line on excerpts; may use {title}, {translator}, {publisher} and
        /// {url}. Required for a mandate.
        #[arg(long)]
        display_notice: Option<String>,
    },
    /// Switch a source off. Every serving view drops it at once.
    Disable {
        /// The source key.
        key: String,
        /// Why.
        #[arg(long)]
        reason: String,
        /// Who is doing it.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Switch a source back on.
    Enable {
        /// The source key.
        key: String,
        /// Who is doing it.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Look up a podcast's feed address from its Apple Podcasts id and print it. Saves it as
    /// feed_url on the source in sources.toml only with --yes, after you have checked it.
    ResolveFeed {
        /// The Apple Podcasts id, such as 1472964846 for Seattle Soto Zen.
        #[arg(long)]
        apple_id: u64,
        /// The feed source in sources.toml.
        #[arg(long, default_value = "ssz-podcast")]
        source: String,
        /// Save the address. Without it nothing is written.
        #[arg(long)]
        yes: bool,
        /// The directory holding sources.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
        /// The lookup API address. For tests.
        #[arg(long, hide = true, default_value = wt_ingest::podcast::APPLE_LOOKUP)]
        lookup_url: String,
    },
}

#[derive(Debug, Subcommand)]
enum IngestCommand {
    /// List each source, or the named one, record new and changed videos and queue them for
    /// classification. Prints counts per source as JSON. Lists metadata only; fetches no media.
    Discover {
        /// Only this source from sources.toml.
        #[arg(long)]
        source: Option<String>,
        /// The directory holding sources.toml, pipeline.toml and series.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// Queue classification again after pipeline.toml, series.toml or the registry's aliases
    /// change: for talks before the gate, and talks classification itself excluded (reopened
    /// when their job runs). Unchanged rules queue nothing. Prints counts as JSON; `wt worker`
    /// runs the jobs. Cleared, pipeline and withdrawn talks are never touched.
    Reclassify {
        /// Only this talk.
        #[arg(long)]
        talk: Option<TalkId>,
        /// Count what would be queued, and queue nothing.
        #[arg(long)]
        dry_run: bool,
        /// The directory holding pipeline.toml and series.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// Compare the Vimeo account list with the talks page and with what discovery recorded,
    /// and write the completeness page. Without WT_VIMEO_ACCESS_TOKEN the account list comes
    /// from account pages saved in a browser (--vimeo-list); the Vimeo site is never fetched.
    /// Prints counts and the ids on the talks page but not in the account list as JSON.
    Completeness {
        /// The Vimeo source in sources.toml.
        #[arg(long, default_value = "ssz-vimeo")]
        source: String,
        /// A saved Vimeo account videos page. Repeat for several pages.
        #[arg(long = "vimeo-list")]
        vimeo_list: Vec<PathBuf>,
        /// Where to write the HTML page.
        #[arg(long)]
        out: PathBuf,
        /// The directory holding sources.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
    /// Write the review page: duplicate clusters, exclusions, review holds, series coverage,
    /// videos by month and donation-looking links. Reads only; it changes nothing.
    Report {
        /// Where to write the HTML page.
        #[arg(long)]
        out: PathBuf,
        /// The directory holding pipeline.toml and series.toml.
        #[arg(long, default_value = "config")]
        config_dir: PathBuf,
    },
}

#[derive(Debug, Args)]
struct WorkerArgs {
    /// Job kinds to run, comma separated: classify, audio, correct, extract, resolve, index.
    #[arg(long, value_delimiter = ',', value_parser = parse_kind, default_value = "classify")]
    kinds: Vec<wt_db::job::JobKind>,
    /// Workers in this process.
    #[arg(long, default_value_t = 1)]
    concurrency: usize,
    /// Exit once no job is due instead of waiting for more.
    #[arg(long)]
    drain: bool,
    /// The directory holding sources.toml, pipeline.toml and series.toml.
    #[arg(long, default_value = "config")]
    config_dir: PathBuf,
}

fn parse_kind(value: &str) -> Result<wt_db::job::JobKind, String> {
    wt_db::job::JobKind::parse(value).ok_or_else(|| {
        let known: Vec<_> = wt_db::job::JobKind::ALL
            .iter()
            .map(|k| k.as_str())
            .collect();
        format!(
            "unknown job kind {value:?}; known kinds are {}",
            known.join(", ")
        )
    })
}

#[derive(Debug, Subcommand)]
enum DbCommand {
    /// Apply every pending migration. Safe to run repeatedly.
    Migrate,
    /// Create or update a login role that inherits one group role. Safe to run repeatedly.
    CreateLogin {
        /// Group role to inherit: wt_owner, wt_pipeline, wt_web or wt_analyst.
        #[arg(long)]
        role: String,
        /// Login role name. Defaults to <role>_login.
        #[arg(long)]
        login: Option<String>,
        /// Environment variable holding the password, for example one ECS injects from Secrets
        /// Manager. The password is never accepted as an argument.
        #[arg(long)]
        password_env: String,
    },
}

#[derive(Debug, Subcommand)]
enum RegistryCommand {
    /// Load sanghas, teachers and aliases from a registry file. Safe to run repeatedly.
    Import {
        /// The registry TOML, for example config/registry.toml.
        #[arg(long)]
        file: PathBuf,
        /// Who is making the change, for correction_log.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
}

#[derive(Debug, Subcommand)]
enum ConsentCommand {
    /// Record a teacher's consent and apply it to their talks. declined and withdrawn run the
    /// teacher takedown, which needs --yes.
    Set {
        /// Display name or alias.
        #[arg(long)]
        teacher: String,
        /// granted, requested, declined, withdrawn or unknown.
        #[arg(long, value_parser = parse_status)]
        status: ConsentStatus,
        /// Pointer to the stored reply, never the reply text.
        #[arg(long)]
        evidence: Option<String>,
        /// Limits the speaker set, in their words.
        #[arg(long)]
        scope_note: Option<String>,
        /// Who is recording it.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// Confirm the takedown a decline or withdrawal runs.
        #[arg(long)]
        yes: bool,
    },
    /// Every teacher with their latest consent and talk counts by state.
    List,
}

#[derive(Debug, Subcommand)]
enum TalkCommand {
    /// Set the rights holder for a talk or every talk of a teacher, and clear any talk that
    /// now passes the gate.
    SetRights {
        #[command(flatten)]
        target: TalkOrTeacher,
        /// The holder's name, or `speaker` for the teacher's display name.
        #[arg(long)]
        holder: String,
        /// Who is making the change.
        #[arg(long, visible_alias = "actor")]
        by: String,
    },
    /// Hold one talk out, which is how a speaker's limits apply. Runs a single-talk takedown,
    /// which needs --yes.
    Exclude {
        /// The talk id.
        #[arg(long)]
        talk: TalkId,
        /// Why, recorded as the talk's state reason.
        #[arg(long)]
        reason: String,
        /// Who is making the change.
        #[arg(long, visible_alias = "actor")]
        by: String,
        /// Confirm the takedown.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct TalkOrTeacher {
    /// One talk, by id.
    #[arg(long)]
    talk: Option<TalkId>,
    /// Every talk of a teacher, by display name or alias.
    #[arg(long)]
    teacher: Option<String>,
}

#[derive(Debug, Args)]
struct TakedownArgs {
    #[command(flatten)]
    target: TalkOrTeacher,
    /// Who asked for it or ran it, for takedown_log.
    #[arg(long, visible_alias = "by")]
    actor: String,
    /// Print the counts and change nothing.
    #[arg(long, conflicts_with = "yes")]
    dry_run: bool,
    /// Confirm a real run.
    #[arg(long)]
    yes: bool,
}

fn parse_basis(value: &str) -> Result<wt_db::rights::Basis, String> {
    wt_db::rights::Basis::parse(value)
        .ok_or_else(|| format!("{value:?} is not one of mandate, license, permission"))
}

fn parse_status(value: &str) -> Result<ConsentStatus, String> {
    ConsentStatus::parse(value).ok_or_else(|| {
        format!("{value:?} is not one of granted, requested, declined, withdrawn, unknown")
    })
}

fn main() -> anyhow::Result<ExitCode> {
    let cli = Cli::parse();
    // The dispatch future holds every command's state, more than the 1 MB main-thread stack a
    // Windows debug build gets, so it lives on the heap.
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?
        .block_on(Box::pin(run(cli)))
}

async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let config = Config::from_env().context("loading configuration")?;
    // Reports go to stdout, so logs must not.
    init_logging(config.env, &config.log, LogOutput::Stderr)?;
    let records = wt_core::records::LocalRecords::new(&cli.records_dir);
    let pool = || async {
        wt_db::connect(&config.database_url)
            .await
            .context("connecting to the database")
    };

    match cli.command {
        Command::Db {
            command: DbCommand::Migrate,
        } => {
            let report = wt_db::migrate(&pool().await?)
                .await
                .context("applying migrations")?;
            tracing::info!(
                applied = report.applied,
                known = report.known,
                "migrations up to date"
            );
        }
        Command::Db {
            command:
                DbCommand::CreateLogin {
                    role,
                    login,
                    password_env,
                },
        } => {
            let login = login.unwrap_or_else(|| format!("{role}_login"));
            let password = std::env::var(&password_env)
                .ok()
                .filter(|p| !p.is_empty())
                .map(wt_core::Secret::new)
                .with_context(|| format!("{password_env} is not set"))?;
            let outcome = wt_db::create_login(&pool().await?, &role, &login, &password)
                .await
                .with_context(|| format!("creating login {login}"))?;
            tracing::info!(login = %login, role = %role, outcome = ?outcome, "login ready");
        }
        Command::Registry {
            command: RegistryCommand::Import { file, by },
        } => commands::registry_import(&pool().await?, &file, &by).await?,
        Command::Consent {
            command:
                ConsentCommand::Set {
                    teacher,
                    status,
                    evidence,
                    scope_note,
                    by,
                    yes,
                },
        } => {
            commands::consent_set(
                &pool().await?,
                &teacher,
                status,
                evidence.as_deref(),
                scope_note.as_deref(),
                &by,
                yes,
            )
            .await?;
        }
        Command::Consent {
            command: ConsentCommand::List,
        } => commands::consent_list(&pool().await?).await?,
        Command::Talk {
            command: TalkCommand::SetRights { target, holder, by },
        } => commands::set_rights(&pool().await?, target.into(), &holder, &by).await?,
        Command::Talk {
            command:
                TalkCommand::Exclude {
                    talk,
                    reason,
                    by,
                    yes,
                },
        } => commands::exclude(&pool().await?, talk, &reason, &by, yes).await?,
        Command::Takedown(args) => {
            commands::takedown(
                &pool().await?,
                &config,
                args.target.into(),
                &args.actor,
                args.dry_run,
                args.yes,
            )
            .await?;
        }
        Command::Audit => return commands::audit(&pool().await?, config.env).await,
        Command::Ingest {
            command: IngestCommand::Discover { source, config_dir },
        } => ingest::discover(&pool().await?, &config, &config_dir, source.as_deref()).await?,
        Command::Ingest {
            command: IngestCommand::Report { out, config_dir },
        } => ingest::report(&pool().await?, &config_dir, &out).await?,
        Command::Ingest {
            command:
                IngestCommand::Completeness {
                    source,
                    vimeo_list,
                    out,
                    config_dir,
                },
        } => {
            ingest::completeness(
                &pool().await?,
                &config,
                &config_dir,
                &source,
                &vimeo_list,
                &out,
            )
            .await?;
        }
        Command::Ingest {
            command:
                IngestCommand::Reclassify {
                    talk,
                    dry_run,
                    config_dir,
                },
        } => ingest::reclassify(&pool().await?, &config_dir, talk, dry_run).await?,
        Command::Source {
            command:
                SourceCommand::ResolveFeed {
                    apple_id,
                    source,
                    yes,
                    config_dir,
                    lookup_url,
                },
        } => {
            source::resolve_feed(
                &pool().await?,
                &config_dir,
                &source,
                apple_id,
                &lookup_url,
                yes,
            )
            .await?
        }
        Command::Source {
            command:
                SourceCommand::Add {
                    key,
                    publisher,
                    prefixes,
                    basis,
                    config_dir,
                    kind,
                    evidence,
                    by,
                    rationale,
                    display_notice,
                },
        } => {
            let basis = match basis {
                Some(basis) => basis,
                None => {
                    let path = config_dir.join("pipeline.toml");
                    wt_ingest::config::RightsSettings::load(&path)
                        .with_context(|| {
                            format!(
                                "reading [rights] default_basis_for_new_sources from {}; \
                                 pass --basis or --config-dir",
                                path.display()
                            )
                        })?
                        .default_basis_for_new_sources
                }
            };
            rights::add(
                &pool().await?,
                &records,
                rights::AddArgs {
                    key: &key,
                    publisher: &publisher,
                    prefixes: &prefixes,
                    basis,
                    kind: &kind,
                    evidence,
                    by: &by,
                    rationale: rationale.as_deref(),
                    display_notice: display_notice.as_deref(),
                },
            )
            .await?;
        }
        Command::Source {
            command: SourceCommand::Disable { key, reason, by },
        } => rights::set_enabled(&pool().await?, &key, false, Some(&reason), &by).await?,
        Command::Source {
            command: SourceCommand::Enable { key, by },
        } => rights::set_enabled(&pool().await?, &key, true, None, &by).await?,
        Command::Rights { command } => match command {
            RightsCommand::Request {
                source,
                to,
                channel,
                draft_file,
                by,
            } => {
                rights::request(
                    &pool().await?,
                    &records,
                    &source,
                    &to,
                    &channel,
                    &draft_file,
                    &by,
                )
                .await?;
            }
            RightsCommand::Sent { to, on, by } => {
                rights::sent(
                    &pool().await?,
                    to.source.as_deref(),
                    to.teacher.as_deref(),
                    on,
                    &by,
                )
                .await?;
            }
            RightsCommand::Grant {
                source,
                statement_file,
                from,
                given_on,
                scope,
                by,
            } => {
                rights::grant(
                    &pool().await?,
                    &records,
                    &source,
                    &statement_file,
                    &from,
                    given_on,
                    &scope,
                    &by,
                )
                .await?;
            }
            RightsCommand::Decline {
                source,
                statement_file,
                by,
                yes,
            } => {
                rights::decline(&pool().await?, &records, &source, &statement_file, &by, yes)
                    .await?;
            }
            RightsCommand::NoResponse { to, by } => {
                rights::no_response(
                    &pool().await?,
                    to.source.as_deref(),
                    to.teacher.as_deref(),
                    &by,
                )
                .await?;
            }
            RightsCommand::SetTalksBasis { basis, by } => {
                rights::set_talks_basis(&pool().await?, &basis, &by).await?;
            }
            RightsCommand::NoticeDraft {
                teacher,
                to,
                channel,
                out,
                from_name,
                contact,
                template,
                by,
            } => {
                rights::notice_draft(
                    &pool().await?,
                    &records,
                    rights::NoticeArgs {
                        teacher: &teacher,
                        to: &to,
                        channel: &channel,
                        out: &out,
                        from_name: from_name.as_deref(),
                        contact: contact.as_deref(),
                        template: &template,
                        by: &by,
                    },
                )
                .await?;
            }
            RightsCommand::Ledger { source, teacher } => {
                rights::ledger(&pool().await?, source.as_deref(), teacher.as_deref()).await?;
            }
            RightsCommand::Due { today } => {
                let today = today.unwrap_or_else(|| chrono::Utc::now().date_naive());
                rights::due(&pool().await?, today).await?;
            }
        },
        Command::Asr { command } => match command {
            AsrCommand::Benchmark(args) => {
                let BenchmarkArgs {
                    clip,
                    reference,
                    talk,
                    references,
                    setups,
                    bench_root,
                    only_setups,
                    only_models,
                    record,
                    note,
                    no_prompt,
                    prompt,
                    config_dir,
                } = *args;
                asr_cmds::benchmark(
                    &pool().await?,
                    &config,
                    &config_dir,
                    asr_cmds::BenchArgs {
                        clip,
                        reference,
                        talks: talk,
                        references,
                        setups_file: setups,
                        bench_root,
                        only_setups,
                        only_models,
                        record,
                        note,
                        no_prompt,
                        prompt,
                    },
                )
                .await?;
            }
            AsrCommand::Requeue { dry_run } => {
                asr_cmds::requeue(&pool().await?, &config, dry_run).await?;
            }
        },
        Command::Text {
            command:
                TextCommand::Import {
                    manifest,
                    estimate: true,
                    concurrency,
                    ..
                },
        } => {
            let tools = wt_ingest::texts::pdf::tools::Poppler::from_env();
            let estimate = wt_ingest::texts::large::estimate(
                &tools,
                &manifest,
                &config.scratch_dir.join("texts"),
                concurrency,
            )?;
            commands::print_json(&estimate)?;
        }
        Command::Text {
            command:
                TextCommand::Import {
                    manifest,
                    apply,
                    by,
                    config_dir,
                    ..
                },
        } => {
            let report = index_cmds::import_text(
                &pool().await?,
                &config,
                &config_dir,
                &manifest,
                apply,
                &by,
            )
            .await?;
            commands::print_json(&report)?;
        }
        Command::Text {
            command: TextCommand::Proof { edition, out_dir },
        } => {
            let path = index_cmds::proof_sheet(&pool().await?, &config, edition, &out_dir).await?;
            commands::print_json(&serde_json::json!({ "sheet": path }))?;
        }
        Command::Text {
            command:
                TextCommand::ProofImport {
                    edition,
                    file,
                    reviewer,
                    config_dir,
                },
        } => {
            let report = index_cmds::proof_import(
                &pool().await?,
                &config,
                edition,
                &file,
                &reviewer,
                &config_dir,
            )
            .await?;
            commands::print_json(&report)?;
        }
        Command::Text {
            command:
                TextCommand::Reocr {
                    edition,
                    pages,
                    dpi,
                    by,
                    config_dir,
                },
        } => {
            let report = index_cmds::reocr(
                &pool().await?,
                &config,
                &config_dir,
                edition,
                &pages,
                dpi,
                &by,
            )
            .await?;
            commands::print_json(&report)?;
        }
        Command::Text {
            command:
                TextCommand::Fix {
                    unit,
                    text,
                    by,
                    reason,
                },
        } => {
            let pool = pool().await?;
            let mut conn = pool.acquire().await?;
            let embedder = stages::embedder(&config)?;
            let report = wt_ingest::texts::proofread::fix_unit(
                &mut conn, &embedder, unit, &text, &by, &reason,
            )
            .await?;
            commands::print_json(&report)?;
        }
        Command::Text {
            command:
                TextCommand::LinkOut {
                    work,
                    url,
                    locators,
                },
        } => {
            let pool = pool().await?;
            let mut conn = pool.acquire().await?;
            let n =
                wt_ingest::texts::reference::link_out(&mut conn, &work, &url, locators.as_deref())
                    .await?;
            commands::print_json(&serde_json::json!({ "work": work, "locators": n }))?;
        }
        Command::Reference {
            command:
                ReferenceCommand::Import {
                    manifest,
                    apply,
                    link_teachers,
                    by,
                    config_dir,
                },
        } => {
            let report = index_cmds::import_reference(
                &pool().await?,
                &config,
                &config_dir,
                &manifest,
                apply,
                link_teachers,
                &by,
            )
            .await?;
            commands::print_json(&report)?;
        }
        Command::Transcript { command } => match command {
            TranscriptCommand::ExportReview {
                talk,
                teacher,
                out,
                config_dir,
            } => {
                let report = index_cmds::export_review(
                    &pool().await?,
                    &config,
                    &config_dir,
                    talk,
                    teacher,
                    &out,
                )
                .await?;
                commands::print_json(&report)?;
            }
            TranscriptCommand::ImportCorrections { file, reviewer } => {
                let report =
                    index_cmds::import_corrections(&pool().await?, &config, &file, &reviewer)
                        .await?;
                commands::print_json(&report)?;
            }
        },
        Command::Pipeline { command } => {
            let pool = pool().await?;
            let mut conn = pool.acquire().await?;
            match command {
                PipelineCommand::Advance => {
                    let queued = wt_ingest::audio::advance_cleared(&mut conn).await?;
                    commands::print_json(
                        &serde_json::json!({ "audio_jobs_queued": queued.len() }),
                    )?;
                }
                PipelineCommand::Retry { talk } => {
                    match wt_db::job::retry_dead(&mut conn, talk).await? {
                        Some(kind) => commands::print_json(
                            &serde_json::json!({ "talk": talk, "requeued": kind.as_str() }),
                        )?,
                        None => anyhow::bail!("talk {talk} has no dead job to retry"),
                    }
                }
            }
        }
        Command::Reindex(args) if args.citations => {
            let report = index_cmds::reindex_citations(&pool().await?, &config).await?;
            commands::print_json(&report)?;
        }
        Command::Reindex(args) => {
            let talks = if args.all { None } else { Some(args.talk) };
            index_cmds::reindex(&pool().await?, &config, &args.config_dir, talks, args.model)
                .await?;
        }
        Command::Export {
            command: ExportCommand::Transcripts { format: _, out },
        } => {
            index_cmds::export_transcripts(&pool().await?, &out).await?;
        }
        Command::Export {
            command:
                ExportCommand::Teacher {
                    teacher,
                    out,
                    config_dir,
                },
        } => {
            let report =
                index_cmds::export_teacher(&pool().await?, &config, &config_dir, &teacher, &out)
                    .await?;
            commands::print_json(&report)?;
        }
        Command::Segment {
            command:
                SegmentCommand::Sample {
                    count,
                    talk,
                    out,
                    config_dir,
                },
        } => {
            let talks = if talk.is_empty() { None } else { Some(talk) };
            index_cmds::segment_sample(&pool().await?, &config, &config_dir, count, talks, &out)
                .await?;
        }
        Command::Glossary { command } => match command {
            GlossaryCommand::Import { file, by } => {
                let text = std::fs::read_to_string(&file)
                    .with_context(|| format!("reading {}", file.display()))?;
                let parsed =
                    wt_registry::glossary::GlossaryFile::parse(&text, &file.display().to_string())?;
                glossary_change(&pool().await?, &config, &parsed.terms, &by).await?;
            }
            GlossaryCommand::Add {
                canonical,
                kind,
                variants,
                by,
            } => {
                let kind = match kind.as_str() {
                    "name" => wt_registry::glossary::TermKind::Name,
                    "text_title" => wt_registry::glossary::TermKind::TextTitle,
                    _ => wt_registry::glossary::TermKind::Term,
                };
                let term = wt_registry::glossary::GlossaryTerm {
                    canonical,
                    kind,
                    variants,
                };
                glossary_change(&pool().await?, &config, &[term], &by).await?;
            }
        },
        Command::Llm {
            command: LlmCommand::Cap { command },
        } => {
            let pool = pool().await?;
            let mut conn = pool.acquire().await?;
            let now = chrono::Utc::now();
            if let CapCommand::Set { usd, by } = &command {
                if !(usd.is_finite() && *usd >= 0.0) {
                    anyhow::bail!("the cap must be a dollar amount of zero or more");
                }
                let released = wt_db::llm::set_cap(&mut conn, *usd, by).await?;
                tracing::info!(cap = usd, released, "language-model cap set");
            }
            let cap = wt_db::llm::cap(&mut conn, config.llm_monthly_cap_usd).await?;
            let spent = wt_db::llm::month_to_date(&mut conn, now).await?;
            commands::print_json(&serde_json::json!({
                "cap_usd": cap,
                "month_to_date_usd": (spent * 10_000.0).round() / 10_000.0,
                "month": wt_db::llm::month_of(now).to_string(),
            }))?;
        }
        Command::Teacher {
            command:
                TeacherCommand::OptOut {
                    teacher,
                    reason,
                    by,
                    suspend_only,
                    yes,
                },
        } => {
            rights::opt_out(
                &pool().await?,
                &config,
                &teacher,
                &reason,
                &by,
                suspend_only,
                yes,
            )
            .await?;
        }
        Command::Teacher {
            command:
                TeacherCommand::Display {
                    teacher,
                    level,
                    talk,
                    reason,
                    by,
                    evidence: _,
                    out,
                    yes,
                    config_dir,
                },
        } if matches!(
            level,
            wt_db::display::DisplayLevel::Removed | wt_db::display::DisplayLevel::ExportThenDelete
        ) =>
        {
            if talk.is_some() {
                anyhow::bail!(
                    "e and f apply to a teacher; to remove one talk use wt takedown --talk"
                );
            }
            if !yes {
                anyhow::bail!(
                    "{level} deletes {teacher}'s passages, segments and transcripts and excludes \
                     them for good; rerun with --yes"
                );
            }
            let pool = pool().await?;
            let export = if level == wt_db::display::DisplayLevel::ExportThenDelete {
                let report =
                    index_cmds::export_teacher(&pool, &config, &config_dir, &teacher, &out).await?;
                if !report.skipped.is_empty() {
                    anyhow::bail!(
                        "the export skipped {} talks whose transcripts could not be read, so \
                         nothing was deleted: {:?}",
                        report.skipped.len(),
                        report.skipped
                    );
                }
                Some(report)
            } else {
                None
            };
            let mut conn = pool.acquire().await?;
            let change =
                wt_db::display::mark_takedown_level(&mut conn, &teacher, level, &by, &reason)
                    .await?;
            drop(conn);
            let takedown = wt_db::rights::opt_out(&pool, &teacher, &reason, &by, false).await?;
            let talks = takedown
                .takedown
                .as_ref()
                .map(|t| t.talks.clone())
                .unwrap_or_default();
            let deleted = stages::delete_talk_objects(&config, &talks)?;
            commands::print_json(&serde_json::json!({
                "level": change,
                "export": export,
                "takedown": takedown,
                "objects_deleted": deleted,
            }))?;
        }
        Command::Teacher {
            command:
                TeacherCommand::Display {
                    teacher,
                    level,
                    talk,
                    reason,
                    by,
                    evidence,
                    ..
                },
        } => {
            let pool = pool().await?;
            let mut conn = pool.acquire().await?;
            let change = wt_db::display::set_level(
                &mut conn,
                &teacher,
                talk,
                level,
                &by,
                &reason,
                evidence.as_deref(),
            )
            .await?;
            commands::print_json(&change)?;
        }
        Command::Exclusion { command } => match command {
            ExclusionCommand::List => rights::exclusions(&pool().await?).await?,
            ExclusionCommand::Lift { id, by, evidence } => {
                rights::lift(&pool().await?, &records, id, &by, &evidence).await?;
            }
        },
        Command::Worker(args) => {
            ingest::worker(
                &pool().await?,
                &config,
                &args.config_dir,
                &args.kinds,
                args.concurrency,
                args.drain,
            )
            .await?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Exactly one of the two, which clap's argument group guarantees.
enum Target {
    Talk(TalkId),
    Teacher(String),
}

impl From<TalkOrTeacher> for Target {
    fn from(args: TalkOrTeacher) -> Self {
        match args.talk {
            Some(talk) => Target::Talk(talk),
            // The group makes --teacher present whenever --talk is absent.
            None => Target::Teacher(args.teacher.unwrap_or_default()),
        }
    }
}

/// Applies a glossary change and queues correction for exactly the talks it affects.
async fn glossary_change(
    pool: &sqlx::PgPool,
    config: &Config,
    terms: &[wt_registry::glossary::GlossaryTerm],
    by: &str,
) -> anyhow::Result<()> {
    let mut conn = pool.acquire().await?;
    let report = wt_db::glossary::import(&mut conn, terms, by).await?;
    let glossary = wt_registry::glossary::Glossary::new(&wt_db::glossary::load(&mut conn).await?)?;
    let objects = stages::objects(config)?;
    let requeued =
        wt_ingest::textjobs::requeue_after_glossary_change(&mut conn, &objects, &glossary).await?;
    commands::print_json(&serde_json::json!({
        "added": report.added,
        "variants_added": report.variants_added,
        "conflicts": report.conflicts,
        "glossary_version": glossary.version(),
        "talks_requeued": requeued.len(),
    }))
}
