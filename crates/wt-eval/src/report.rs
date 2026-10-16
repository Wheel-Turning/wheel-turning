//! `wt eval report` (Spec 07, command 5): the share of questions that pass, claim-audit counts,
//! the teacher-count distribution and thin coverage, each named-teacher query, stage latency
//! percentiles and dana event counts, with the PRD verdicts. Draft questions are reported apart
//! and count toward no verdict until the owner approves them.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;
use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::answer::SummaryStatus;

use crate::EvalError;
use crate::run::{self, RunAnswer};

/// `question_pass_ratio`'s default: a question passes when at least this share of its rated
/// passages are both faithful and relevant.
pub const QUESTION_PASS_RATIO: f64 = 0.67;

/// One question's line.
#[derive(Debug, Clone, Serialize)]
pub struct QuestionLine {
    /// `q01` and so on.
    pub id: String,
    /// The question.
    pub text: String,
    /// Its tags.
    pub tags: Vec<String>,
    /// Not yet approved by the owner.
    pub draft: bool,
    /// Passages returned.
    pub passages: usize,
    /// Distinct teachers.
    pub teachers: usize,
    /// Passage ratings with both answers.
    pub rated: usize,
    /// Of those, faithful and relevant.
    pub good: usize,
    /// `None` when nothing is rated.
    pub passes: Option<bool>,
    /// The answer's notice, if any.
    pub notice: Option<String>,
    /// Whole-path latency.
    pub total_ms: u64,
}

/// A named-teacher query's outcome.
#[derive(Debug, Clone, Serialize)]
pub struct NamedLine {
    /// `q01` and so on.
    pub id: String,
    /// The question.
    pub text: String,
    /// `named_teacher` when a teacher was detected, else `standard`.
    pub mode: String,
    /// Who was detected.
    pub teacher: Option<String>,
    /// Passages returned.
    pub passages: usize,
    /// `named_none`, `named_few` or none.
    pub notice: Option<String>,
}

/// Latency percentiles for one stage, in milliseconds.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct Percentiles {
    /// Median.
    pub p50: u64,
    /// 90th percentile.
    pub p90: u64,
    /// 95th percentile.
    pub p95: u64,
    /// Slowest.
    pub max: u64,
}

/// A verdict the PRD names.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The test passed.
    Pass,
    /// The test failed.
    Fail,
    /// Nothing rated yet.
    NotRated,
}

/// The whole report.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// The run.
    pub run: Uuid,
    /// `name@version`.
    pub set: String,
    /// Build identifier.
    pub build: String,
    /// Git commit, if recorded.
    pub commit: Option<String>,
    /// Model names, as recorded.
    pub models: serde_json::Value,
    /// When it started.
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// `None` if it stopped partway.
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Questions answered, of the set's size.
    pub answered: usize,
    /// The set's size.
    pub set_size: usize,
    /// The ratio used.
    pub question_pass_ratio: f64,
    /// Reviewers whose ratings are counted.
    pub reviewers: Vec<String>,
    /// Approved questions that pass.
    pub questions_passing: usize,
    /// Approved questions in the run.
    pub questions_counted: usize,
    /// Draft questions that pass, reported apart.
    pub draft_passing: usize,
    /// Draft questions in the run.
    pub draft_questions: usize,
    /// Fewer than half of approved questions passing is a retrieval fail.
    pub retrieval: Verdict,
    /// Claim-audit counts by verdict.
    pub claims: BTreeMap<String, i64>,
    /// Any invented claim surviving is a synthesis fail.
    pub synthesis: Verdict,
    /// Answers by teacher count.
    pub teacher_counts: BTreeMap<usize, usize>,
    /// Answers with the thin-coverage notice.
    pub thin_coverage: usize,
    /// Answers whose reranker was unavailable.
    pub rerank_degraded: usize,
    /// Answers whose synthesis failed or timed out.
    pub synthesis_failed: usize,
    /// Answers by how their summary settled (Spec 11, PB2.4); `unrecorded` for answers stored
    /// before statuses.
    pub summary_statuses: BTreeMap<String, usize>,
    /// Themes shown across the run.
    pub themes_shown: usize,
    /// Themes dropped by the filters and verifier.
    pub themes_dropped: usize,
    /// Each named-teacher query: tagged `named_teacher`, or run in named mode.
    pub named: Vec<NamedLine>,
    /// Stage latency percentiles, in stage order.
    pub latency: Vec<(String, Percentiles)>,
    /// Dana events on the run's answers, by kind.
    pub dana: BTreeMap<String, i64>,
    /// Excerpt length and sentence boundaries (Spec 05, Answer assembly, rule 8).
    pub excerpts: ExcerptStats,
    /// Per question.
    pub questions: Vec<QuestionLine>,
}

/// How the run's excerpts came out, measured on the passages as they serve now. An answer stored
/// before excerpts counts its whole passages, so an older run gives the baseline.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ExcerptStats {
    /// Excerpts measured.
    pub count: usize,
    /// Their mean length in words.
    pub mean_words: f64,
    /// Excerpts starting or ending away from a stored sentence boundary.
    pub mid_sentence: usize,
    /// That share of `count`.
    pub mid_sentence_share: f64,
    /// Excerpts whose first word follows no sentence punctuation or whose last word ends
    /// without it, a stricter reading for text with little punctuation.
    pub unpunctuated_edge: usize,
    /// Excerpts by the step that chose them; `whole` for an answer without spans.
    pub by_chooser: BTreeMap<String, usize>,
    /// The same passages shown whole, as before excerpts: their mean length in words...
    pub whole_mean_words: f64,
    /// ...and the share starting or ending mid-sentence.
    pub whole_mid_sentence_share: f64,
}

/// Measures each answer's excerpts against the windows serving now.
async fn excerpt_stats(pool: &PgPool, answers: &[RunAnswer]) -> Result<ExcerptStats, EvalError> {
    let mut out = ExcerptStats::default();
    let mut words = 0usize;
    let (mut whole_words, mut whole_mid) = (0usize, 0usize);
    for a in answers {
        let windows = wt_db::answer::windows(pool, &a.payload.passage_ids).await?;
        for w in &windows {
            let Some(selected) = w.passages.get(w.selected) else {
                continue;
            };
            let span = a.payload.span(selected.id);
            let located = span.and_then(|s| w.locate(s));
            let (start, end) = located.unwrap_or_else(|| w.selected_range());
            let chooser = match (span, located) {
                (Some(s), Some(_)) => s.chooser.as_str(),
                (Some(_), None) => "withdrawn_neighbor",
                (None, _) => "whole",
            };
            *out.by_chooser.entry(chooser.to_owned()).or_default() += 1;
            out.count += 1;
            words += end - start;
            let (mid_start, mid_end) = w.mid_sentence(start, end);
            if mid_start || mid_end {
                out.mid_sentence += 1;
            }
            if unpunctuated_edge(w, start, end) {
                out.unpunctuated_edge += 1;
            }
            let (a, b) = w.selected_range();
            whole_words += b - a;
            let (ms, me) = w.mid_sentence(a, b);
            if ms || me {
                whole_mid += 1;
            }
        }
    }
    #[allow(clippy::cast_precision_loss)]
    if out.count > 0 {
        out.mean_words = words as f64 / out.count as f64;
        out.mid_sentence_share = out.mid_sentence as f64 / out.count as f64;
        out.whole_mean_words = whole_words as f64 / out.count as f64;
        out.whole_mid_sentence_share = whole_mid as f64 / out.count as f64;
    }
    Ok(out)
}

/// Whether window words `start..end` begin after a word without sentence punctuation (or with
/// a lower-case word at the window's start) or end on one.
fn unpunctuated_edge(w: &wt_core::excerpt::Window, start: usize, end: usize) -> bool {
    use wt_core::excerpt::ends_sentence;
    let text = w.text(0, w.len());
    let words: Vec<&str> = text.split_whitespace().collect();
    let starts_clean = match start.checked_sub(1) {
        Some(i) => words.get(i).is_some_and(|p| ends_sentence(p)),
        None => words
            .first()
            .and_then(|f| f.chars().find(|c| c.is_alphanumeric()))
            .is_some_and(char::is_uppercase),
    };
    let ends_clean = end
        .checked_sub(1)
        .and_then(|i| words.get(i))
        .is_some_and(|l| ends_sentence(l));
    !(starts_clean && ends_clean)
}

/// Builds the report for a run.
pub async fn build(pool: &PgPool, run: Uuid, pass_ratio: f64) -> Result<Report, EvalError> {
    let stored = run::find(pool, run).await?;
    let answers = run::results(pool, run).await?;

    let ratings = sqlx::query!(
        "select question_id, reviewer, faithful, relevant from eval_rating where run_id = $1",
        run
    )
    .fetch_all(pool)
    .await?;
    let mut reviewers: Vec<String> = ratings.iter().map(|r| r.reviewer.clone()).collect();
    reviewers.sort();
    reviewers.dedup();
    let mut by_question: BTreeMap<Uuid, (usize, usize)> = BTreeMap::new();
    for r in &ratings {
        let e = by_question.entry(r.question_id).or_default();
        e.0 += 1;
        if r.faithful && r.relevant {
            e.1 += 1;
        }
    }

    let claim_rows = sqlx::query!(
        r#"select verdict, count(*) as "n!" from claim_audit where run_id = $1 group by verdict"#,
        run
    )
    .fetch_all(pool)
    .await?;
    let mut claims: BTreeMap<String, i64> = ["supported", "drifting", "invented"]
        .iter()
        .map(|v| ((*v).to_owned(), 0))
        .collect();
    for c in claim_rows {
        claims.insert(c.verdict, c.n);
    }
    let audited: i64 = claims.values().sum();
    let synthesis = if audited == 0 {
        Verdict::NotRated
    } else if claims.get("invented").copied().unwrap_or(0) > 0 {
        Verdict::Fail
    } else {
        Verdict::Pass
    };

    let dana_rows = sqlx::query!(
        r#"select d.kind, count(*) as "n!" from dana_event d
           join eval_result r on r.answer_id = d.answer_id
           where r.run_id = $1 group by d.kind"#,
        run
    )
    .fetch_all(pool)
    .await?;
    let dana = dana_rows.into_iter().map(|d| (d.kind, d.n)).collect();

    let questions: Vec<QuestionLine> = answers
        .iter()
        .map(|a| {
            let (rated, good) = by_question.get(&a.question_id).copied().unwrap_or((0, 0));
            #[allow(clippy::cast_precision_loss)]
            let passes = (rated > 0).then_some(good as f64 >= pass_ratio * rated as f64);
            QuestionLine {
                id: a.label(),
                text: a.text.clone(),
                tags: a.tags.clone(),
                draft: a.is_draft(),
                passages: a.payload.passage_ids.len(),
                teachers: a.outcome.as_ref().map_or(0, |o| o.teacher_count),
                rated,
                good,
                passes,
                notice: a.payload.notice.clone(),
                total_ms: a.timings.total_ms,
            }
        })
        .collect();
    let counted = questions.iter().filter(|q| !q.draft).count();
    let passing = questions
        .iter()
        .filter(|q| !q.draft && q.passes == Some(true))
        .count();
    let draft_questions = questions.len() - counted;
    let draft_passing = questions
        .iter()
        .filter(|q| q.draft && q.passes == Some(true))
        .count();
    let any_rated = questions.iter().any(|q| !q.draft && q.rated > 0);
    // The PRD's line: fewer than half of the questions passing is a retrieval fail. A question
    // with nothing rated does not pass.
    let retrieval = if !any_rated {
        Verdict::NotRated
    } else if passing * 2 < counted {
        Verdict::Fail
    } else {
        Verdict::Pass
    };

    let mut teacher_counts = BTreeMap::new();
    for q in &questions {
        *teacher_counts.entry(q.teachers).or_insert(0) += 1;
    }
    let outcomes = || answers.iter().filter_map(|a| a.outcome.as_ref());

    Ok(Report {
        run,
        set: stored.set.to_string(),
        build: stored.build,
        commit: stored
            .config
            .get("commit")
            .and_then(|c| c.as_str())
            .map(str::to_owned),
        models: stored
            .config
            .get("models")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        started_at: stored.started_at,
        finished_at: stored.finished_at,
        answered: answers.len(),
        set_size: stored.set_size,
        question_pass_ratio: pass_ratio,
        reviewers,
        questions_passing: passing,
        questions_counted: counted,
        draft_passing,
        draft_questions,
        retrieval,
        claims,
        synthesis,
        teacher_counts,
        thin_coverage: answers.iter().filter(|a| a.thin_coverage).count(),
        rerank_degraded: outcomes().filter(|o| o.rerank_degraded).count(),
        synthesis_failed: outcomes().filter(|o| o.synthesis_failed).count(),
        summary_statuses: summary_statuses(&answers),
        themes_shown: outcomes().map(|o| o.theme_count).sum(),
        themes_dropped: outcomes().map(|o| o.themes_dropped).sum(),
        named: named(&answers),
        latency: latency(&answers),
        dana,
        excerpts: excerpt_stats(pool, &answers).await?,
        questions,
    })
}

/// Answers by summary status.
fn summary_statuses(answers: &[RunAnswer]) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for a in answers {
        let status = a.summary_status.as_deref().unwrap_or(UNRECORDED);
        *out.entry(status.to_owned()).or_insert(0) += 1;
    }
    out
}

/// The status of an answer stored before `answer.summary_status`.
const UNRECORDED: &str = "unrecorded";

/// The summary-status counts as one line, in the order of [`SummaryStatus::ALL`]; statuses with
/// no answers are left out.
fn status_line(counts: &BTreeMap<String, usize>) -> String {
    let names = SummaryStatus::ALL
        .iter()
        .map(|s| s.as_str())
        .chain(std::iter::once(UNRECORDED));
    let parts: Vec<String> = names
        .filter_map(|n| counts.get(n).map(|c| format!("{n} {c}")))
        .collect();
    if parts.is_empty() {
        "none".to_owned()
    } else {
        parts.join(", ")
    }
}

/// The report's lines for named-teacher questions: those tagged `named_teacher` or answered in that
/// mode.
fn named(answers: &[RunAnswer]) -> Vec<NamedLine> {
    answers
        .iter()
        .filter(|a| {
            a.tags.iter().any(|t| t == "named_teacher") || a.payload.mode == "named_teacher"
        })
        .map(|a| NamedLine {
            id: a.label(),
            text: a.text.clone(),
            mode: a.payload.mode.clone(),
            teacher: a.named_teacher.clone(),
            passages: a.payload.passage_ids.len(),
            notice: a.payload.notice.clone(),
        })
        .collect()
}

/// Nearest-rank percentile of sorted values.
fn percentile(sorted: &[u64], p: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (p * sorted.len()).div_ceil(100).max(1);
    sorted[rank.min(sorted.len()) - 1]
}

/// Percentiles of a list of values.
pub fn percentiles(values: &[u64]) -> Percentiles {
    let mut v = values.to_vec();
    v.sort_unstable();
    Percentiles {
        p50: percentile(&v, 50),
        p90: percentile(&v, 90),
        p95: percentile(&v, 95),
        max: v.last().copied().unwrap_or(0),
    }
}

/// Latency percentiles per query stage across the run's answers.
fn latency(answers: &[RunAnswer]) -> Vec<(String, Percentiles)> {
    type Get = fn(&wt_api::ask::StageTimings) -> u64;
    let stages: [(&str, Get); 10] = [
        ("parse", |t| t.parse_ms),
        ("embed", |t| t.embed_ms),
        ("search", |t| t.search_ms),
        ("rerank", |t| t.rerank_ms),
        ("select", |t| t.select_ms),
        ("load", |t| t.load_ms),
        ("excerpt", |t| t.excerpt_ms),
        ("synth", |t| t.synth_ms),
        ("persist", |t| t.persist_ms),
        ("total", |t| t.total_ms),
    ];
    stages
        .iter()
        .map(|(name, get)| {
            let values: Vec<u64> = answers.iter().map(|a| get(&a.timings)).collect();
            ((*name).to_owned(), percentiles(&values))
        })
        .collect()
}

/// A verdict as the report prints it.
fn verdict(v: Verdict) -> &'static str {
    match v {
        Verdict::Pass => "pass",
        Verdict::Fail => "FAIL",
        Verdict::NotRated => "not rated yet",
    }
}

/// The report as plain text for the terminal.
pub fn render(r: &Report) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "Evaluation run {} over {}", r.run, r.set);
    let _ = writeln!(
        s,
        "Build {}{}; started {}; {}",
        r.build,
        r.commit
            .as_deref()
            .map(|c| format!(" at {c}"))
            .unwrap_or_default(),
        r.started_at.format("%Y-%m-%d %H:%M UTC"),
        match r.finished_at {
            Some(f) => format!("finished {}", f.format("%Y-%m-%d %H:%M UTC")),
            None => "did not finish".to_owned(),
        }
    );
    let _ = writeln!(s, "Models: {}", r.models);
    let _ = writeln!(s, "Answered {} of {} questions", r.answered, r.set_size);
    let _ = writeln!(s);

    let _ = writeln!(s, "Retrieval");
    let reviewers = if r.reviewers.is_empty() {
        "none yet".to_owned()
    } else {
        r.reviewers.join(", ")
    };
    let _ = writeln!(s, "  Reviewers: {reviewers}");
    let _ = writeln!(
        s,
        "  Questions passing (at least {:.0}% of rated passages faithful and relevant): {} of {}",
        r.question_pass_ratio * 100.0,
        r.questions_passing,
        r.questions_counted
    );
    if r.draft_questions > 0 {
        let _ = writeln!(
            s,
            "  Draft questions, not counted until the owner approves them: {} passing of {}",
            r.draft_passing, r.draft_questions
        );
    }
    let _ = writeln!(
        s,
        "  PRD verdict (fewer than half passing fails): {}",
        verdict(r.retrieval)
    );
    let _ = writeln!(s);

    let _ = writeln!(s, "Synthesis");
    let _ = writeln!(
        s,
        "  Themes shown {}, dropped by filters and verifier {}, synthesis failed on {} answers",
        r.themes_shown, r.themes_dropped, r.synthesis_failed
    );
    let _ = writeln!(
        s,
        "  Summary status by answer: {}",
        status_line(&r.summary_statuses)
    );
    let _ = writeln!(
        s,
        "  Claim audit: {} supported, {} drifting, {} invented",
        r.claims.get("supported").copied().unwrap_or(0),
        r.claims.get("drifting").copied().unwrap_or(0),
        r.claims.get("invented").copied().unwrap_or(0)
    );
    let _ = writeln!(
        s,
        "  PRD verdict (any invented claim surviving fails): {}",
        verdict(r.synthesis)
    );
    let _ = writeln!(s);

    let _ = writeln!(s, "Coverage");
    let counts: Vec<String> = r
        .teacher_counts
        .iter()
        .map(|(t, n)| format!("{t} teachers: {n}"))
        .collect();
    let _ = writeln!(s, "  Answers by teacher count: {}", counts.join(", "));
    let _ = writeln!(s, "  Thin-coverage answers: {}", r.thin_coverage);
    let _ = writeln!(
        s,
        "  Reranker unavailable on: {} answers",
        r.rerank_degraded
    );
    let _ = writeln!(s);

    let e = &r.excerpts;
    let _ = writeln!(s, "Excerpts");
    let _ = writeln!(
        s,
        "  {} excerpts, mean {:.1} words; starting or ending mid-sentence: {} ({:.1}%, under 2% passes)",
        e.count,
        e.mean_words,
        e.mid_sentence,
        e.mid_sentence_share * 100.0
    );
    let _ = writeln!(
        s,
        "  An edge without sentence punctuation: {}",
        e.unpunctuated_edge
    );
    let _ = writeln!(
        s,
        "  The same passages shown whole: mean {:.1} words, {:.1}% mid-sentence",
        e.whole_mean_words,
        e.whole_mid_sentence_share * 100.0
    );
    let choosers: Vec<String> = e
        .by_chooser
        .iter()
        .map(|(k, n)| format!("{k} {n}"))
        .collect();
    let _ = writeln!(s, "  Chosen by: {}", choosers.join(", "));
    let _ = writeln!(s);

    let _ = writeln!(s, "Named-teacher queries");
    if r.named.is_empty() {
        let _ = writeln!(s, "  none in this set");
    }
    for n in &r.named {
        let _ = writeln!(
            s,
            "  {} {}: {} {}, {} passages{}",
            n.id,
            n.text,
            n.mode,
            n.teacher.as_deref().unwrap_or("(no teacher detected)"),
            n.passages,
            n.notice
                .as_deref()
                .map(|x| format!(", notice {x}"))
                .unwrap_or_default()
        );
    }
    let _ = writeln!(s);

    let _ = writeln!(
        s,
        "Stage latency, ms (latency is reported, not a pass or fail)"
    );
    let _ = writeln!(
        s,
        "  {:<8} {:>7} {:>7} {:>7} {:>7}",
        "stage", "p50", "p90", "p95", "max"
    );
    for (name, p) in &r.latency {
        let _ = writeln!(
            s,
            "  {:<8} {:>7} {:>7} {:>7} {:>7}",
            name, p.p50, p.p90, p.p95, p.max
        );
    }
    let _ = writeln!(s);

    let dana: Vec<String> = r.dana.iter().map(|(k, n)| format!("{k} {n}")).collect();
    let _ = writeln!(
        s,
        "Dana events: {}",
        if dana.is_empty() {
            "none".to_owned()
        } else {
            dana.join(", ")
        }
    );
    let _ = writeln!(s);

    let _ = writeln!(s, "Questions");
    for q in &r.questions {
        let pass = match q.passes {
            Some(true) => "passes",
            Some(false) => "fails",
            None => "unrated",
        };
        let _ = writeln!(
            s,
            "  {}{} {} teachers, {} passages, {} of {} rated good, {pass}, {} ms{}",
            q.id,
            if q.draft { " (draft)" } else { "" },
            q.teachers,
            q.passages,
            q.good,
            q.rated,
            q.total_ms,
            q.notice
                .as_deref()
                .map(|n| format!(", {n}"))
                .unwrap_or_default()
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_the_nearest_rank() {
        let v: Vec<u64> = (1..=20).collect();
        let p = percentiles(&v);
        assert_eq!(
            p,
            Percentiles {
                p50: 10,
                p90: 18,
                p95: 19,
                max: 20
            }
        );
        assert_eq!(percentiles(&[]), Percentiles::default());
        assert_eq!(percentiles(&[7]).p95, 7);
    }
}
