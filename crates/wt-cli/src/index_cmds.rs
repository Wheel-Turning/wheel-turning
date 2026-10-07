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
            Ok(SampleOutcome::Ran(Steps::Segmented { seg, audio_ms, .. })) => {
                render_talk(title, *talk, &seg, audio_ms)
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
