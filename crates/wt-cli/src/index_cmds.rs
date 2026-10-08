//! `wt reindex`, `wt export transcripts` and `wt segment sample` (Spec 04, Spec 07).

use std::fmt::Write as _;
use std::path::Path;

use anyhow::Context;
use sqlx::PgPool;
use wt_core::Config;
use wt_core::ids::TalkId;
use wt_db::job::JobKind;
use wt_ingest::audio::{SampleOutcome, Steps};
use wt_ingest::config::ClassifyConfig;
use wt_ingest::textjobs;
use wt_ml::Embedder;
use wt_registry::glossary::Glossary;
use wt_segment::{Segmentation, SpanKind};

use crate::commands;
use crate::stages::{self, Stages};

/// Reruns the index step for `talks`, or for every corrected talk, with the configured
/// embedding model or `model`. A talk whose passages are already current is left alone.
pub async fn reindex(
    pool: &PgPool,
    config: &Config,
    config_dir: &Path,
    talks: Option<Vec<TalkId>>,
    model: Option<String>,
) -> anyhow::Result<()> {
    let mut config = config.clone();
    if let Some(m) = model {
        config.embed_model = m;
    }
    let embedder = stages::embedder(&config)?;
    let chunk = stages::chunk_settings(config_dir)?;
    let objects = stages::objects(&config)?;
    let mut conn = pool.acquire().await?;
    let glossary = Glossary::new(&wt_db::glossary::load(&mut conn).await?)?;
    let talks = match talks {
        Some(t) => t,
        None => wt_db::transcript::corrected_talks(&mut conn).await?,
    };
    let (mut written, mut current, mut refused) = (0, 0, Vec::new());
    for talk in talks {
        match textjobs::run_index(
            &mut conn,
            talk,
            &objects,
            &glossary,
            &chunk,
            &embedder as &dyn Embedder,
        )
        .await?
        {
            Ok(i) if i.written => written += 1,
            Ok(_) => current += 1,
            Err(gate) => {
                refused.push(serde_json::json!({ "talk": talk, "gate": gate.to_string() }))
            }
        }
    }
    if written > 0 {
        wt_index::index::analyze(&mut conn).await?;
    }
    commands::print_json(&serde_json::json!({
        "model": embedder.model_id(),
        "reindexed": written,
        "already_current": current,
        "not_cleared": refused,
    }))
}

/// Writes `passages.jsonl` under `out`: one line per servable passage, with no names.
pub async fn export_transcripts(pool: &PgPool, out: &Path) -> anyhow::Result<()> {
    let mut conn = pool.acquire().await?;
    let rows = wt_db::export::servable_passages(&mut conn).await?;
    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    let path = out.join("passages.jsonl");
    let mut text = String::new();
    for r in &rows {
        text.push_str(&serde_json::to_string(r)?);
        text.push('\n');
    }
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    let talks: std::collections::BTreeSet<_> = rows.iter().map(|r| r.talk_id).collect();
    commands::print_json(&serde_json::json!({
        "file": path.display().to_string(),
        "passages": rows.len(),
        "talks": talks.len(),
    }))
}

/// Runs the audio steps on up to `count` cleared talks without persisting anything and writes
/// the segmentation report to `out`.
pub async fn segment_sample(
    pool: &PgPool,
    config: &Config,
    config_dir: &Path,
    count: i64,
    talks: Option<Vec<TalkId>>,
    out: &Path,
) -> anyhow::Result<()> {
    let cfg = ClassifyConfig::load(
        &config_dir.join("pipeline.toml"),
        &config_dir.join("series.toml"),
    )?;
    let stages = Stages::build(
        pool,
        config,
        config_dir,
        &cfg,
        &[JobKind::Audio],
        "wt segment sample",
    )
    .await?;
    let mut conn = pool.acquire().await?;
    let picked: Vec<(TalkId, String)> = match talks {
        Some(ids) => {
            let mut v = Vec::new();
            for id in ids {
                let title = wt_db::transcript::title(&mut conn, id)
                    .await?
                    .unwrap_or_default();
                v.push((id, title));
            }
            v
        }
        None => wt_db::transcript::sample_cleared(&mut conn, count).await?,
    };
    let mut sections = Vec::new();
    for (talk, title) in &picked {
        tracing::info!(%talk, "sampling segmentation");
        let section = match stages.sample(&mut conn, *talk).await {
            Ok(SampleOutcome::Ran(Steps::Segmented {
                seg,
                audio_ms,
                fallback_windows,
                ..
            })) => {
                let mut html = render_talk(title, *talk, &seg, audio_ms);
                if fallback_windows > 0 {
                    html = html.replacen(
                        "unaligned words</p>",
                        &format!(
                            "unaligned words · {fallback_windows} ambiguous windows labeled by rule, not by the model</p>"
                        ),
                        1,
                    );
                }
                html
            }
            Ok(SampleOutcome::Ran(Steps::Held(reason))) => {
                note(title, *talk, &format!("Held for review: {reason}"))
            }
            Ok(SampleOutcome::NoMedia) => note(title, *talk, "No media route"),
            Ok(SampleOutcome::NotCleared(gate)) => {
                note(title, *talk, &format!("Not cleared: {gate}"))
            }
            Err(e) => note(title, *talk, &format!("Failed: {e:#}")),
        };
        sections.push(section);
    }
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(out, page(&sections)).with_context(|| format!("writing {}", out.display()))?;
    commands::print_json(&serde_json::json!({
        "report": out.display().to_string(),
        "talks": picked.len(),
    }))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn clock(ms: i32) -> String {
    let s = ms / 1000;
    format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

fn color(kind: SpanKind) -> &'static str {
    match kind {
        SpanKind::Talk => "var(--talk)",
        SpanKind::QaTeacher => "var(--qa)",
        SpanKind::Chant => "var(--chant)",
        SpanKind::Announcement => "var(--announce)",
        SpanKind::Sitting => "var(--sitting)",
        SpanKind::Other => "var(--other)",
    }
}

fn note(title: &str, talk: TalkId, message: &str) -> String {
    format!(
        "<section><h2>{}</h2><p class=\"id\">{}</p><p class=\"note\">{}</p></section>",
        esc(title),
        talk.as_uuid(),
        esc(message)
    )
}

/// One talk: a timeline of labeled spans and student turns, then a table of spans with the
/// first words of each. Student turns are shown as time ranges only.
fn render_talk(title: &str, talk: TalkId, seg: &Segmentation, audio_ms: i64) -> String {
    let total = audio_ms.max(1) as f64;
    let pct = |ms: i32| f64::from(ms) * 100.0 / total;
    let mut bar = String::new();
    for span in &seg.spans {
        let _ = write!(
            bar,
            "<div class=\"span\" style=\"left:{:.3}%;width:{:.3}%;background:{}\" title=\"{} {}–{}\"></div>",
            pct(span.start_ms),
            pct(span.end_ms - span.start_ms).max(0.1),
            color(span.kind),
            span.kind.as_str(),
            clock(span.start_ms),
            clock(span.end_ms)
        );
    }
    let mut students = String::new();
    for (s, e) in &seg.student_turns {
        let _ = write!(
            students,
            "<div class=\"span\" style=\"left:{:.3}%;width:{:.3}%;background:var(--student)\" title=\"student {}–{}\"></div>",
            pct(*s),
            pct(e - s).max(0.1),
            clock(*s),
            clock(*e)
        );
    }
    let mut rows = String::new();
    for span in &seg.spans {
        let first = span
            .words
            .map(|(a, b)| {
                seg.words[a..b.min(a + 14)]
                    .iter()
                    .map(|w| w.text.trim())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let _ = write!(
            rows,
            "<tr><td><span class=\"dot\" style=\"background:{}\"></span>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            color(span.kind),
            span.kind.as_str(),
            clock(span.start_ms),
            clock(span.end_ms),
            esc(&first)
        );
    }
    for (s, e) in &seg.student_turns {
        let _ = write!(
            rows,
            "<tr class=\"student\"><td><span class=\"dot\" style=\"background:var(--student)\"></span>student turn</td><td>{}</td><td>{}</td><td>(not kept)</td></tr>",
            clock(*s),
            clock(*e)
        );
    }
    format!(
        "<section><h2>{}</h2><p class=\"id\">{} · teacher {} · {} segments · {} student turns · {} unaligned words</p>\
         <div class=\"bar\">{bar}</div><div class=\"bar thin\">{students}</div>\
         <table><thead><tr><th>Label</th><th>Start</th><th>End</th><th>First words</th></tr></thead><tbody>{rows}</tbody></table></section>",
        esc(title),
        talk.as_uuid(),
        esc(&seg.teacher),
        seg.segments.len(),
        seg.student_turns.len(),
        seg.unaligned_words
    )
}

fn page(sections: &[String]) -> String {
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Segmentation sample</title>
<style>
:root {{ --bg:#fbfaf7; --fg:#22211f; --muted:#6b675f; --line:#e3dfd6;
  --talk:#2f6f4f; --qa:#4f8fbf; --chant:#c08a2e; --announce:#9a5fb0; --sitting:#b9b4a8; --other:#d9d4c8; --student:#c0504d; }}
@media (prefers-color-scheme: dark) {{ :root {{ --bg:#1b1a18; --fg:#ecebe6; --muted:#a29d93; --line:#3a3833; }} }}
body {{ background:var(--bg); color:var(--fg); font:15px/1.5 system-ui, sans-serif; margin:0 auto; max-width:1100px; padding:24px 16px; }}
h1 {{ font-size:22px; }} h2 {{ font-size:17px; margin:32px 0 2px; }}
.id, .note {{ color:var(--muted); font-size:13px; margin:0 0 8px; }}
.bar {{ position:relative; height:22px; background:var(--line); border-radius:4px; overflow:hidden; }}
.bar.thin {{ height:8px; margin-top:3px; }}
.span {{ position:absolute; top:0; bottom:0; }}
table {{ border-collapse:collapse; width:100%; margin-top:10px; font-size:13px; }}
td, th {{ text-align:left; padding:4px 8px; border-bottom:1px solid var(--line); vertical-align:top; }}
.dot {{ display:inline-block; width:9px; height:9px; border-radius:50%; margin-right:6px; }}
.legend span {{ margin-right:14px; font-size:13px; }}
</style></head><body>
<h1>Segmentation sample</h1>
<p class="legend"><span><span class="dot" style="background:var(--talk)"></span>talk</span><span><span class="dot" style="background:var(--qa)"></span>qa_teacher</span><span><span class="dot" style="background:var(--chant)"></span>chant</span><span><span class="dot" style="background:var(--announce)"></span>announcement</span><span><span class="dot" style="background:var(--sitting)"></span>sitting</span><span><span class="dot" style="background:var(--other)"></span>other</span><span><span class="dot" style="background:var(--student)"></span>student turn (time only)</span></p>
<p class="note">Nothing in this report was stored. It holds teachers' words: keep it under data/ and do not commit it.</p>
{}
</body></html>
"#,
        sections.join("\n")
    )
}

/// `wt export teacher`: the teacher's talks for their own use.
pub async fn export_teacher(
    pool: &PgPool,
    config: &Config,
    config_dir: &Path,
    teacher: &str,
    out: &Path,
) -> anyhow::Result<wt_ingest::owner_export::ExportReport> {
    let paragraphs = stages::chunk_settings(config_dir)?;
    let objects = stages::objects(config)?;
    let mut conn = pool.acquire().await?;
    let glossary = Glossary::new(&wt_db::glossary::load(&mut conn).await?)?;
    Ok(wt_ingest::owner_export::export_teacher(
        &mut conn,
        &objects,
        &glossary,
        &paragraphs,
        teacher,
        out,
    )
    .await?)
}

/// `wt transcript export-review`: one review file per talk.
pub async fn export_review(
    pool: &PgPool,
    config: &Config,
    config_dir: &Path,
    talk: Option<TalkId>,
    teacher: Option<String>,
    out: &Path,
) -> anyhow::Result<serde_json::Value> {
    let paragraphs = stages::chunk_settings(config_dir)?;
    let objects = stages::objects(config)?;
    let mut conn = pool.acquire().await?;
    let glossary = Glossary::new(&wt_db::glossary::load(&mut conn).await?)?;
    let talks = match (talk, teacher) {
        (Some(t), _) => vec![t],
        (None, Some(name)) => {
            let (id, _) = wt_db::registry::find_teacher(&mut conn, &name).await?;
            wt_db::transcript::teacher_transcribed_talks(&mut conn, id)
                .await?
                .into_iter()
                .map(|t| t.id)
                .collect()
        }
        (None, None) => anyhow::bail!("give --talk or --teacher"),
    };
    let mut files = Vec::new();
    for talk in talks {
        let (path, file) = wt_ingest::review::export_review(
            &mut conn,
            &objects,
            &glossary,
            &paragraphs,
            talk,
            out,
        )
        .await
        .with_context(|| format!("writing the review file for talk {talk}"))?;
        files.push(serde_json::json!({
            "talk": talk,
            "file": path.display().to_string(),
            "paragraphs": file.paragraphs.len(),
        }));
    }
    Ok(serde_json::json!({ "files": files }))
}

/// `wt transcript import-corrections`.
pub async fn import_corrections(
    pool: &PgPool,
    config: &Config,
    file: &Path,
    reviewer: &str,
) -> anyhow::Result<wt_ingest::review::ImportReport> {
    let text =
        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let corrections: wt_ingest::review::Corrections =
        serde_json::from_str(&text).with_context(|| format!("reading {}", file.display()))?;
    let objects = stages::objects(config)?;
    let mut conn = pool.acquire().await?;
    let glossary = Glossary::new(&wt_db::glossary::load(&mut conn).await?)?;
    Ok(wt_ingest::review::import_corrections(
        &mut conn,
        &objects,
        &glossary,
        &corrections,
        reviewer,
    )
    .await?)
}

/// `wt text import`: the embedder loads only for `--apply`, so a dry run needs no model.
pub async fn import_text(
    pool: &PgPool,
    config: &Config,
    config_dir: &Path,
    manifest: &Path,
    apply: bool,
    by: &str,
) -> anyhow::Result<wt_ingest::texts::ImportReport> {
    let licenses = wt_ingest::config::LicenseSettings::load(&config_dir.join("pipeline.toml"))?;
    let objects = stages::objects(config)?;
    let mut conn = pool.acquire().await?;
    let fake = wt_ml::FakeEmbedder::new("dry-run");
    let real;
    let embedder: &dyn Embedder = if apply {
        real = stages::embedder(config)?;
        &real
    } else {
        &fake
    };
    let texts = wt_ingest::config::TextSettings::load(&config_dir.join("pipeline.toml"))?;
    let tools = wt_ingest::texts::pdf::tools::Poppler::from_env();
    let env = wt_ingest::texts::PdfEnv {
        tools: &tools,
        scratch: config.scratch_dir.join("texts"),
        word_list: texts.word_list,
    };
    let report = wt_ingest::texts::import(
        &mut conn,
        &objects,
        embedder,
        manifest,
        &licenses.allowed_license_codes,
        by,
        apply,
        Some(&env),
    )
    .await?;
    if let Some(reason) = report.pdf.as_ref().and_then(|p| p.stopped.as_ref()) {
        commands::print_json(&report)?;
        anyhow::bail!("the import stopped: {reason}");
    }
    Ok(report)
}

/// `wt takedown --edition | --source | --url`: texts out of every store. A real run needs
/// `--yes`; without it the counts print and nothing changes.
pub async fn takedown_texts(
    pool: &PgPool,
    config: &Config,
    scope: &wt_db::text_takedown::TextScope,
    reason: &str,
    actor: &str,
    dry_run: bool,
    yes: bool,
) -> anyhow::Result<()> {
    let objects = stages::objects(config)?;
    let mut conn = pool.acquire().await?;
    let real = !dry_run && yes;
    let report =
        wt_ingest::texts::takedown::take_down(&mut conn, &objects, scope, reason, actor, !real)
            .await?;
    commands::print_json(&report)?;
    if !dry_run && !yes {
        anyhow::bail!(
            "a takedown removes {} edition(s), counted above, and excludes them permanently; \
             nothing was changed. Rerun with --yes",
            report.editions.len()
        );
    }
    Ok(())
}

/// The scope a request's handling acts on: an edition or source named, or the edition of the
/// reported excerpt.
async fn request_scope(
    conn: &mut sqlx::PgConnection,
    request: &wt_db::text_takedown::Request,
    edition: Option<sqlx::types::Uuid>,
    source: Option<String>,
) -> anyhow::Result<wt_db::text_takedown::TextScope> {
    use wt_db::text_takedown::TextScope;
    if let Some(s) = source {
        return Ok(TextScope::Source(s));
    }
    if let Some(e) = edition {
        return Ok(TextScope::Edition(e));
    }
    let unit = request
        .text_unit_id
        .context("the request names no excerpt; give --edition or --source")?;
    let e = wt_db::text_takedown::edition_of_unit(conn, unit)
        .await?
        .context("the reported excerpt is gone; give --edition or --source")?;
    Ok(TextScope::Edition(e))
}

/// `wt takedown requests ...`.
pub async fn takedown_requests(
    pool: &PgPool,
    config: &Config,
    command: crate::RequestsCommand,
) -> anyhow::Result<()> {
    use crate::RequestsCommand as R;
    use wt_db::text_takedown::{self as td, TextScope};
    let mut conn = pool.acquire().await?;
    let find = |r: Option<td::Request>, id| r.with_context(|| format!("no request {id}"));
    match command {
        R::List { all } => commands::print_json(&td::requests(&mut conn, all).await?)?,
        R::Show { id } => commands::print_json(&find(td::request(&mut conn, id).await?, id)?)?,
        R::Suspend {
            id,
            edition,
            source,
            by,
        } => {
            let request = find(td::request(&mut conn, id).await?, id)?;
            let reason = format!("removal request {id}");
            let action = match request_scope(&mut conn, &request, edition, source).await? {
                TextScope::Source(key) => {
                    wt_db::rights::set_enabled(pool, &key, false, Some(&reason), &by).await?;
                    format!("source {key} disabled")
                }
                TextScope::Edition(e) => {
                    wt_ingest::texts::takedown::quarantine(&mut conn, e, &reason, &by).await?;
                    format!("edition {e} quarantined")
                }
                TextScope::UrlPrefix(_) => anyhow::bail!("suspend an edition or a source"),
            };
            td::handle_request(&mut conn, id, "suspended", &action, &by).await?;
            commands::print_json(&serde_json::json!({ "request": id, "action": action }))?;
        }
        R::Remove {
            id,
            edition,
            source,
            by,
            yes,
        } => {
            let request = find(td::request(&mut conn, id).await?, id)?;
            let scope = request_scope(&mut conn, &request, edition, source).await?;
            let reason = format!("removal request {id}");
            let objects = stages::objects(config)?;
            let report = wt_ingest::texts::takedown::take_down(
                &mut conn, &objects, &scope, &reason, &by, !yes,
            )
            .await?;
            commands::print_json(&report)?;
            if !yes {
                anyhow::bail!("a removal is permanent; nothing was changed. Rerun with --yes");
            }
            let action = format!("{} edition(s) taken down", report.editions.len());
            td::handle_request(&mut conn, id, "removed", &action, &by).await?;
        }
        R::Decline { id, by, reason } => {
            let request = find(td::request(&mut conn, id).await?, id)?;
            // A suspension made for this request is undone.
            if request.status == "suspended"
                && let Some(action) = &request.action_taken
            {
                if let Some(key) = action
                    .strip_prefix("source ")
                    .and_then(|r| r.strip_suffix(" disabled"))
                {
                    wt_db::rights::set_enabled(pool, key, true, None, &by).await?;
                } else if let Some(e) = action
                    .strip_prefix("edition ")
                    .and_then(|r| r.strip_suffix(" quarantined"))
                    .and_then(|e| e.parse().ok())
                {
                    wt_ingest::texts::takedown::release(&mut conn, e, &by).await?;
                }
            }
            td::handle_request(&mut conn, id, "declined", &reason, &by).await?;
            commands::print_json(&serde_json::json!({ "request": id, "status": "declined" }))?;
        }
    }
    Ok(())
}

/// `wt text reocr`.
pub async fn reocr(
    pool: &PgPool,
    config: &Config,
    config_dir: &Path,
    edition: sqlx::types::Uuid,
    pages: &str,
    dpi: u32,
    by: &str,
) -> anyhow::Result<wt_ingest::texts::ImportReport> {
    let range = wt_ingest::texts::pdf::skipped(&[pages.to_owned()])?
        .first()
        .copied()
        .context("--pages names no pages")?;
    let pipeline = config_dir.join("pipeline.toml");
    let licenses = wt_ingest::config::LicenseSettings::load(&pipeline)?;
    let texts = wt_ingest::config::TextSettings::load(&pipeline)?;
    let objects = stages::objects(config)?;
    let embedder = stages::embedder(config)?;
    let tools = wt_ingest::texts::pdf::tools::Poppler::from_env();
    let env = wt_ingest::texts::PdfEnv {
        tools: &tools,
        scratch: config.scratch_dir.join("texts"),
        word_list: texts.word_list,
    };
    let mut conn = pool.acquire().await?;
    Ok(wt_ingest::texts::large::reocr(
        &mut conn,
        &objects,
        &embedder,
        &env,
        &licenses.allowed_license_codes,
        edition,
        range,
        dpi,
        by,
    )
    .await?)
}

/// `wt text proof`: writes the proof sheet and returns its path.
pub async fn proof_sheet(
    pool: &PgPool,
    config: &Config,
    edition: sqlx::types::Uuid,
    out_dir: &Path,
) -> anyhow::Result<std::path::PathBuf> {
    let objects = stages::objects(config)?;
    let tools = wt_ingest::texts::pdf::tools::Poppler::from_env();
    let mut conn = pool.acquire().await?;
    let (_, html) = wt_ingest::texts::proofread::sheet(
        &mut conn,
        &objects,
        &tools,
        edition,
        &config.scratch_dir.join("texts"),
    )
    .await?;
    std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let path = out_dir.join(format!("proof-{edition}.html"));
    std::fs::write(&path, html).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// `wt text proof-import`.
pub async fn proof_import(
    pool: &PgPool,
    config: &Config,
    edition: sqlx::types::Uuid,
    file: &Path,
    reviewer: &str,
    config_dir: &Path,
) -> anyhow::Result<wt_ingest::texts::proofread::ProofReport> {
    let texts = wt_ingest::config::TextSettings::load(&config_dir.join("pipeline.toml"))?;
    let proof: wt_ingest::texts::pdf::proof::ProofFile = serde_json::from_slice(
        &std::fs::read(file).with_context(|| format!("reading {}", file.display()))?,
    )
    .with_context(|| format!("reading {}", file.display()))?;
    let objects = stages::objects(config)?;
    let mut conn = pool.acquire().await?;
    Ok(wt_ingest::texts::proofread::import_proof(
        &mut conn,
        &objects,
        edition,
        &proof,
        reviewer,
        wt_ingest::texts::proofread::Thresholds {
            random_max: texts.proof_random_max_errors,
            worst_max: texts.proof_worst_max_errors,
        },
    )
    .await?)
}

/// `wt reference import`; a glossary change then queues correction for the talks it affects.
pub async fn import_reference(
    pool: &PgPool,
    config: &Config,
    config_dir: &Path,
    manifest: &Path,
    apply: bool,
    link_teachers: bool,
    by: &str,
) -> anyhow::Result<serde_json::Value> {
    let licenses = wt_ingest::config::LicenseSettings::load(&config_dir.join("pipeline.toml"))?;
    let objects = stages::objects(config)?;
    let mut conn = pool.acquire().await?;
    let (report, glossary_terms) = wt_ingest::texts::reference::import(
        &mut conn,
        &objects,
        manifest,
        &licenses.allowed_license_codes,
        by,
        apply,
        link_teachers,
    )
    .await?;
    let mut requeued = 0;
    if report.applied && !glossary_terms.is_empty() {
        let glossary = Glossary::new(&wt_db::glossary::load(&mut conn).await?)?;
        requeued =
            wt_ingest::textjobs::requeue_after_glossary_change(&mut conn, &objects, &glossary)
                .await?
                .len();
    }
    let mut value = serde_json::to_value(&report)?;
    if let Some(map) = value.as_object_mut() {
        map.insert("talks_requeued".into(), serde_json::json!(requeued));
    }
    Ok(value)
}

/// `wt reindex --citations`: links citations to text units, and queues resolution again for
/// talks with staged citations, keyed by the hand list, so a new locator list or text resolves
/// citations that matched nothing before.
pub async fn reindex_citations(
    pool: &PgPool,
    config: &Config,
) -> anyhow::Result<serde_json::Value> {
    use wt_core::objects::ObjectStore as _;
    let objects = stages::objects(config)?;
    let mut conn = pool.acquire().await?;
    let linked = wt_db::reference::link_citations(&mut conn).await?;
    let version = wt_db::reference::locator_version(&mut conn).await?;
    let mut queued = 0;
    for talk in wt_db::transcript::corrected_talks(&mut conn).await? {
        if objects
            .get(&wt_ingest::textjobs::staging_key(talk))?
            .is_some()
            && wt_db::job::enqueue(
                &mut conn,
                wt_db::job::JobKind::Resolve,
                Some(talk),
                &format!("locators-{version}"),
                &serde_json::json!({ "reason": "reindex --citations" }),
            )
            .await?
            .is_some()
        {
            queued += 1;
        }
    }
    Ok(serde_json::json!({ "citations_relinked": linked, "resolve_queued": queued }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn a_talk_renders_its_spans_first_words_and_student_turns_as_times_only() {
        use wt_core::transcript::Word;
        use wt_segment::{SegmentDraft, Span};
        let words: Vec<Word> = ["We", "sit", "together", "today", "Yes", "exactly", "so"]
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let ms = i32::try_from(i).unwrap() * 1000 + 130_000;
                Word::new(*w, ms, ms + 500, 0.9)
            })
            .collect();
        let span = |kind, start_ms, end_ms, words| Span {
            kind,
            start_ms,
            end_ms,
            words,
        };
        let seg = Segmentation {
            teacher: "S1".into(),
            words,
            spans: vec![
                span(SpanKind::Sitting, 0, 125_000, None),
                span(SpanKind::Talk, 130_000, 134_000, Some((0, 4))),
                span(SpanKind::QaTeacher, 140_000, 143_000, Some((4, 7))),
            ],
            segments: vec![SegmentDraft {
                kind: SpanKind::Talk,
                start_ms: 130_000,
                end_ms: 134_000,
                words: (0, 4),
                text: "We sit together today".into(),
            }],
            student_turns: vec![(135_000, 139_000)],
            unaligned_words: 0,
        };
        let talk = TalkId::from(sqlx::types::Uuid::nil());
        let html = page(&[render_talk("Fixture <talk>", talk, &seg, 150_000)]);
        assert!(html.contains("Fixture &lt;talk&gt;"));
        assert!(html.contains(">sitting</td><td>0:00:00</td><td>0:02:05<"));
        assert!(html.contains("We sit together today"));
        assert!(html.contains(">qa_teacher</td>"));
        assert!(html.contains("student turn</td><td>0:02:15</td><td>0:02:19</td><td>(not kept)"));
        assert_eq!(html.matches("<section>").count(), 1);
    }

    #[test]
    fn the_page_escapes_titles() {
        let html = page(&[note(
            "<b>Q&A</b>",
            TalkId::from(sqlx::types::Uuid::nil()),
            "x",
        )]);
        assert!(html.contains("&lt;b&gt;Q&amp;A&lt;/b&gt;"));
        assert!(!html.contains("<b>Q&A"));
    }
}
