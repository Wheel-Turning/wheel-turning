//! `wt eval export-review` (Spec 07, command 3): one self-contained HTML file for the teacher
//! reviewer, in the Gate A notebook's format so the reviewer uses one interface for both. Each
//! question lists its passages with teacher, talk and source link, faithful and relevant radios
//! and a note; each theme is a claim row with supported, drifting or invented. The inline script
//! keeps ratings in the browser and saves them as JSON. Nothing loads from the network.
//!
//! Passage text is read through `servable_passage`; a passage that stopped serving after the
//! run is shown as withdrawn and is not rated.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::ids::{PassageId, TalkId};
use wt_db::answer::Served;

use crate::EvalError;
use crate::run::{self, RunAnswer};

const CSS: &str = include_str!("../../../scripts/notebooks/review.css");
const JS: &str = include_str!("../assets/review.js");
const EXTRA_CSS: &str = ".c{border-left:3px solid #999;padding-left:.75rem;margin:1rem 0}\
.claim{font-style:italic}.withdrawn{color:#666}";

/// Writes the review page for a run. `lead_in_ms` is `[answer] link_lead_in_ms`.
pub async fn export(pool: &PgPool, run: Uuid, lead_in_ms: i64) -> Result<String, EvalError> {
    let stored = run::find(pool, run).await?;
    let answers = run::results(pool, run).await?;

    let mut served: BTreeMap<Uuid, Vec<Served>> = BTreeMap::new();
    let mut talks = BTreeSet::new();
    for a in &answers {
        let s = wt_db::answer::passages(pool, &a.payload.passage_ids).await?;
        talks.extend(s.iter().map(|p| p.talk.id));
        served.insert(a.answer_id, s);
    }
    let context = neighbors(pool, &talks).await?;
    Ok(render(
        &stored.id.to_string(),
        &answers,
        &served,
        &context,
        lead_in_ms,
    ))
}

type Context = BTreeMap<PassageId, (Option<String>, Option<String>)>;

/// The passage before and after each passage of these talks, for the reviewer's context.
async fn neighbors(pool: &PgPool, talks: &BTreeSet<TalkId>) -> Result<Context, EvalError> {
    let mut out = Context::new();
    for talk in talks {
        if let Some((_, ps)) = wt_db::answer::talk(pool, *talk).await? {
            for (i, p) in ps.iter().enumerate() {
                let before = i
                    .checked_sub(1)
                    .and_then(|j| ps.get(j))
                    .map(|x| x.text.clone());
                let after = ps.get(i + 1).map(|x| x.text.clone());
                out.insert(p.id, (before, after));
            }
        }
    }
    Ok(out)
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// The page itself, from data already read. Public so tests can render without a database.
pub fn render(
    run_id: &str,
    answers: &[RunAnswer],
    served: &BTreeMap<Uuid, Vec<Served>>,
    context: &Context,
    lead_in_ms: i64,
) -> String {
    let run = esc(run_id);
    let mut h = String::new();
    let _ = write!(
        h,
        "<!doctype html><html lang='en'><head><meta charset='utf-8'>\
         <meta name='viewport' content='width=device-width, initial-scale=1'>\
         <title>Evaluation review {run}</title><style>{CSS}{EXTRA_CSS}</style></head>\
         <body data-run='{run}'><main><h1>Evaluation review</h1>\
         <section class='brief'><p>Each question below is something a practitioner might ask. \
         Under it are the passages the search returned, transcribed from talks. Each passage \
         names its teacher and talk and links to the source at that point.</p>\
         <p>For each passage, answer two questions:</p><ul>\
         <li><strong>Faithful</strong>: read as it stands, does the passage represent what the \
         teacher meant? Answer no if it is cut so that it misleads, garbled, or says something \
         the teacher would not stand behind. The surrounding text is under each passage.</li>\
         <li><strong>Relevant</strong>: would it help the person who asked?</li></ul>\
         <p>Some answers also have short summary lines, each citing passages by number. For \
         each line, choose <strong>supported</strong> if the cited passages say everything in \
         it, <strong>drifting</strong> if it stretches or shades what they say, and \
         <strong>invented</strong> if it says something they do not.</p>\
         <p>Please rate unflatteringly. A doubtful passage is a no, and a doubtful line is not \
         supported. The test only means something if what falls short is marked so.</p>\
         <p>Ratings are kept in this browser as you go. When you finish, press Save ratings \
         and send the file back.</p></section><form onsubmit='return false'>"
    );
    for a in answers {
        let qid = esc(&a.label());
        let _ = write!(h, "<section class='q'><h2>{qid}. {}</h2>", esc(&a.text));
        if !a.tags.is_empty() {
            let _ = write!(h, "<p class='tags'>{}</p>", esc(&a.tags.join(", ")));
        }
        let shown = served.get(&a.answer_id).map(Vec::as_slice).unwrap_or(&[]);
        if a.payload.passage_ids.is_empty() {
            h.push_str("<p>No passages were returned for this question.</p>");
        }
        for (n, id) in a.payload.passage_ids.iter().enumerate() {
            let n = n + 1;
            let Some(p) = shown.iter().find(|p| p.id == *id) else {
                let _ = write!(
                    h,
                    "<p class='withdrawn'>{n}. This passage has been withdrawn.</p>"
                );
                continue;
            };
            passage(&mut h, &qid, n, p, context, lead_in_ms);
        }
        for (t, theme) in a.payload.themes.iter().enumerate() {
            let cites: Vec<String> = theme
                .passage_ids
                .iter()
                .filter_map(|id| a.payload.passage_ids.iter().position(|p| p == id))
                .map(|i| (i + 1).to_string())
                .collect();
            let name = format!("v-{qid}-{t}");
            let _ = write!(
                h,
                "<div class='c' data-q='{qid}' data-a='{}' data-t='{t}'>\
                 <div class='who'>Summary line {} (cites passages {})</div>\
                 <p class='claim'>{}</p><fieldset><legend>This line is</legend>",
                a.answer_id,
                t + 1,
                esc(&cites.join(", ")),
                esc(&theme.description)
            );
            for (value, word) in [
                ("supported", "Supported"),
                ("drifting", "Drifting"),
                ("invented", "Invented"),
            ] {
                let _ = write!(
                    h,
                    "<label><input type='radio' name='{name}' value='{value}'> {word}</label>"
                );
            }
            let _ = write!(
                h,
                "</fieldset><label for='cn-{qid}-{t}' class='who'>Note</label>\
                 <textarea id='cn-{qid}-{t}'></textarea></div>"
            );
        }
        h.push_str("</section>");
    }
    let _ = write!(
        h,
        "</form></main><div class='bar'><label>Your name <input type='text' id='reviewer'>\
         </label><span id='done' role='status' aria-live='polite'></span>\
         <button type='button' id='download'>Save ratings</button></div>\
         <script>{JS}</script></body></html>"
    );
    h
}

fn passage(h: &mut String, qid: &str, n: usize, p: &Served, context: &Context, lead_in_ms: i64) {
    let pid = esc(&p.id.to_string());
    let when = p
        .talk
        .date
        .map(|d| format!(", {}", d.format("%Y-%m-%d")))
        .unwrap_or_default();
    let _ = write!(
        h,
        "<div class='p' data-q='{qid}' data-p='{pid}'><div class='who'>{n}. {}: {}{}</div>\
         <blockquote>{}</blockquote>",
        esc(&p.talk.teacher_name),
        esc(&p.talk.title),
        esc(&when),
        esc(&p.text)
    );
    let link = wt_api::view::source_link(&p.talk, p.start_ms, lead_in_ms);
    if link.href.starts_with("https://") {
        let _ = write!(
            h,
            "<p class='src'><a href='{}' target='_blank' rel='noopener noreferrer'>{}</a>",
            esc(&link.href),
            esc(&link.label)
        );
        if let Some(at) = &link.starts_at {
            let _ = write!(h, " {}", esc(at));
        }
        h.push_str("</p>");
    } else {
        let at = link.starts_at.as_deref().unwrap_or(&link.label);
        let _ = write!(h, "<p class='src'>{}</p>", esc(at));
    }
    if let Some((before, after)) = context.get(&p.id)
        && (before.is_some() || after.is_some())
    {
        h.push_str("<details><summary>Surrounding text</summary>");
        if let Some(b) = before {
            let _ = write!(h, "<p><em>Before:</em> {}</p>", esc(b));
        }
        if let Some(a) = after {
            let _ = write!(h, "<p><em>After:</em> {}</p>", esc(a));
        }
        h.push_str("</details>");
    }
    for (key, word) in [("f", "Faithful"), ("r", "Relevant")] {
        let name = format!("{key}-{qid}-{pid}");
        let _ = write!(
            h,
            "<fieldset><legend>{word}</legend>\
             <label><input type='radio' name='{name}' value='yes'> Yes</label>\
             <label><input type='radio' name='{name}' value='no'> No</label></fieldset>"
        );
    }
    let _ = write!(
        h,
        "<label for='n-{qid}-{pid}' class='who'>Note</label>\
         <textarea id='n-{qid}-{pid}'></textarea></div>"
    );
}
