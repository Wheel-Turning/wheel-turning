//! The reports `wt report` prints (Spec 06, Logging, rule 6). Each reads only what the analyst
//! role may read: the aggregate views and `flagged_queries()`. Run them on a pool connected as
//! `wt_analyst`, which cannot read an unflagged query's text at all.

use std::collections::BTreeMap;

use sqlx::PgPool;
use sqlx::types::chrono::{DateTime, NaiveDate, Utc};

use crate::DbError;

/// Whether this connection can read `query_log` directly. `wt report` refuses to run when it
/// can, so a report is never run as a role that could read unflagged questions.
pub async fn reads_query_log(pool: &PgPool) -> Result<bool, DbError> {
    Ok(
        sqlx::query_scalar!(r#"select has_table_privilege('query_log', 'select') as "reads!""#)
            .fetch_one(pool)
            .await?,
    )
}

/// One row of `v_answer_stats`: answers per day, mode and teacher count.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AnswerStats {
    /// The day.
    pub day: DateTime<Utc>,
    /// `standard` or `named_teacher`.
    pub mode: String,
    /// Teachers in the answer.
    pub teacher_count: i32,
    /// Answers.
    pub answers: i64,
    /// Mean latency.
    pub avg_latency_ms: Option<i32>,
    /// Answers with the thin-coverage notice.
    pub thin_answers: i64,
}

/// Answers per day since `since`.
pub async fn answers(pool: &PgPool, since: DateTime<Utc>) -> Result<Vec<AnswerStats>, DbError> {
    let rows = sqlx::query!(
        r#"select day as "day!", mode as "mode!", teacher_count as "teacher_count!",
                  answers as "answers!", avg_latency_ms, thin_answers as "thin_answers!"
           from v_answer_stats where day >= date_trunc('day', $1::timestamptz)
           order by day, mode, teacher_count"#,
        since
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| AnswerStats {
            day: r.day,
            mode: r.mode,
            teacher_count: r.teacher_count,
            answers: r.answers,
            avg_latency_ms: r.avg_latency_ms,
            thin_answers: r.thin_answers,
        })
        .collect())
}

/// One row of `v_dana_clicks`: dana events per day and kind.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DanaEvents {
    /// The day.
    pub day: DateTime<Utc>,
    /// `panel_open`, `split_computed` or `link_click`.
    pub kind: String,
    /// Events.
    pub events: i64,
}

/// Dana events per day and kind. With a small group, read link clicks as a floor test, not a
/// conversion rate (PRD, Dana mechanic).
pub async fn dana(pool: &PgPool) -> Result<Vec<DanaEvents>, DbError> {
    let rows = sqlx::query!(
        r#"select day as "day!", kind as "kind!", events as "events!"
           from v_dana_clicks order by day, kind"#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| DanaEvents {
            day: r.day,
            kind: r.kind,
            events: r.events,
        })
        .collect())
}

/// One row of `v_consent_coverage`: talks per state, consent status and teacher.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Coverage {
    /// The talk state.
    pub state: String,
    /// The consent status.
    pub consent_status: String,
    /// The teacher, when the talk has one.
    pub teacher: Option<String>,
    /// Talks.
    pub talks: i64,
}

/// Talks per state, consent status and teacher.
pub async fn coverage(pool: &PgPool) -> Result<Vec<Coverage>, DbError> {
    let rows = sqlx::query!(
        r#"select state::text as "state!", consent_status::text as "consent_status!", teacher,
                  talks as "talks!"
           from v_consent_coverage order by teacher nulls last, state, consent_status"#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Coverage {
            state: r.state,
            consent_status: r.consent_status,
            teacher: r.teacher,
            talks: r.talks,
        })
        .collect())
}

/// A query its tester flagged, the only kind whose text may be read (Spec 06, Logging).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Flagged {
    /// When it was flagged.
    pub flagged_at: Option<DateTime<Utc>>,
    /// When it was asked.
    pub asked_at: DateTime<Utc>,
    /// `standard` or `named_teacher`.
    pub mode: String,
    /// Teachers in the answer.
    pub teacher_count: i32,
    /// The question; absent if a privacy purge removed it.
    pub query_text: Option<String>,
}

/// Every flagged query, through `flagged_queries()`.
pub async fn flagged(pool: &PgPool) -> Result<Vec<Flagged>, DbError> {
    let rows = sqlx::query!(
        r#"select flagged_at, created_at as "created_at!", mode as "mode!",
                  teacher_count as "teacher_count!", query_text
           from flagged_queries() order by flagged_at"#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Flagged {
            flagged_at: r.flagged_at,
            asked_at: r.created_at,
            mode: r.mode,
            teacher_count: r.teacher_count,
            query_text: r.query_text,
        })
        .collect())
}

/// Questions by category, relevance and whether the gate was degraded, from `v_query_triage`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TriageRow {
    /// `none`, `medical`, `distress`, `out_of_scope` or `acute`.
    pub category: String,
    /// `practice`, `adjacent`, `unrelated`, or absent when the gate did not answer.
    pub relevance: Option<String>,
    /// The gate was off, failed, timed out or found the cap reached.
    pub triage_degraded: bool,
    /// Questions.
    pub questions: i64,
    /// Of those, answered: an unrelated question answered was searched anyway.
    pub answered: i64,
    /// Answers that found nothing.
    pub none_found: i64,
    /// Answers with a Not relevant vote.
    pub not_relevant: i64,
    /// Flagged questions.
    pub flagged: i64,
}

/// What each layer said, summed from `triage_tally`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LayerRow {
    /// The rules' level.
    pub rules: String,
    /// The exemplars' level, `down` or `skipped`.
    pub embedding: String,
    /// The gate's risk, or `unanswered`.
    pub risk: String,
    /// The level that risk maps to, or `unanswered`.
    pub gate_level: String,
    /// The gate's relevance, or `unanswered`.
    pub relevance: String,
    /// The gate's confidence, or `unanswered`.
    pub confidence: String,
    /// The level the question got.
    pub level: String,
    /// Questions.
    pub n: i64,
}

/// Totals and rates over the period.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct TriageTotals {
    /// Questions.
    pub questions: i64,
    /// With `triage_degraded`.
    pub triage_degraded: i64,
    /// Given `out_of_scope`.
    pub out_of_scope: i64,
    /// Given relevance `unrelated` at level none.
    pub unrelated: i64,
    /// Of those, searched anyway.
    pub unrelated_searched_anyway: i64,
    /// `triage_degraded` over questions.
    pub triage_degraded_rate: f64,
    /// `out_of_scope` over questions.
    pub out_of_scope_rate: f64,
    /// Questions shown the outside-these-talks page, over questions.
    pub unrelated_rate: f64,
    /// Answers.
    pub answered: i64,
    /// Answers that found nothing, over answers.
    pub none_found_rate: f64,
    /// Answers with a Not relevant vote, over answers.
    pub not_relevant_rate: f64,
    /// Flagged questions, over questions.
    pub flagged_rate: f64,
    /// Gate answers whose level differs from the rules'.
    pub gate_vs_rules: i64,
    /// Gate answers whose level differs from the exemplars', where the exemplars ran.
    pub gate_vs_exemplars: i64,
    /// Questions the gate raised above both other layers.
    pub gate_raised: i64,
}

/// `wt sensitive report`: counts only, never question text.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TriageReport {
    /// The start of the period.
    pub since: DateTime<Utc>,
    /// Totals and rates.
    pub totals: TriageTotals,
    /// Questions by category, relevance and degradation.
    pub by_category: Vec<TriageRow>,
    /// Layer combinations where the gate disagreed with the rules or the exemplars.
    pub disagreements: Vec<LayerRow>,
}

/// A sensitivity level's order from none to acute, or none for an unknown name.
fn rank(level: &str) -> Option<u8> {
    ["none", "medical", "distress", "out_of_scope", "acute"]
        .iter()
        .position(|l| *l == level)
        .and_then(|i| u8::try_from(i).ok())
}

/// `n` as a share of `of`, zero when `of` is.
fn rate(n: i64, of: i64) -> f64 {
    if of == 0 { 0.0 } else { n as f64 / of as f64 }
}

/// Counts since `since` from `v_query_triage` and `triage_tally`, both readable by
/// `wt_analyst`.
pub async fn triage(pool: &PgPool, since: DateTime<Utc>) -> Result<TriageReport, DbError> {
    let rows = sqlx::query!(
        r#"select sensitive_category as "category!", relevance,
                  triage_degraded as "triage_degraded!",
                  sum(questions)::bigint as "questions!", sum(answered)::bigint as "answered!",
                  sum(none_found)::bigint as "none_found!",
                  sum(not_relevant)::bigint as "not_relevant!", sum(flagged)::bigint as "flagged!"
           from v_query_triage where day >= date_trunc('day', $1::timestamptz)
           group by 1, 2, 3 order by 1, 2 nulls last, 3"#,
        since
    )
    .fetch_all(pool)
    .await?;
    let by_category: Vec<TriageRow> = rows
        .into_iter()
        .map(|r| TriageRow {
            category: r.category,
            relevance: r.relevance,
            triage_degraded: r.triage_degraded,
            questions: r.questions,
            answered: r.answered,
            none_found: r.none_found,
            not_relevant: r.not_relevant,
            flagged: r.flagged,
        })
        .collect();
    let layers = sqlx::query!(
        r#"select rules as "rules!", embedding as "embedding!", risk as "risk!",
                  gate_level as "gate_level!", relevance as "relevance!",
                  confidence as "confidence!", level as "level!", sum(n)::bigint as "n!"
           from triage_tally where day >= ($1::timestamptz)::date
           group by 1, 2, 3, 4, 5, 6, 7 order by 8 desc, 1, 2, 3, 4, 5, 6, 7"#,
        since
    )
    .fetch_all(pool)
    .await?;

    let mut t = TriageTotals::default();
    let mut not_relevant = 0;
    let mut none_found = 0;
    let mut flagged = 0;
    for r in &by_category {
        t.questions += r.questions;
        t.answered += r.answered;
        none_found += r.none_found;
        not_relevant += r.not_relevant;
        flagged += r.flagged;
        if r.triage_degraded {
            t.triage_degraded += r.questions;
        }
        if r.category == "out_of_scope" {
            t.out_of_scope += r.questions;
        }
        if r.category == "none" && r.relevance.as_deref() == Some("unrelated") {
            t.unrelated += r.questions - r.answered;
            t.unrelated_searched_anyway += r.answered;
        }
    }
    let mut disagreements = Vec::new();
    for r in layers {
        let row = LayerRow {
            rules: r.rules,
            embedding: r.embedding,
            risk: r.risk,
            gate_level: r.gate_level,
            relevance: r.relevance,
            confidence: r.confidence,
            level: r.level,
            n: r.n,
        };
        let Some(gate) = rank(&row.gate_level) else {
            continue;
        };
        let rules = rank(&row.rules);
        let embedding = rank(&row.embedding);
        let vs_rules = rules != Some(gate);
        let vs_exemplars = embedding.is_some_and(|e| e != gate);
        if vs_rules {
            t.gate_vs_rules += row.n;
        }
        if vs_exemplars {
            t.gate_vs_exemplars += row.n;
        }
        if rules.into_iter().chain(embedding).all(|l| l < gate) {
            t.gate_raised += row.n;
        }
        if vs_rules || vs_exemplars {
            disagreements.push(row);
        }
    }
    t.triage_degraded_rate = rate(t.triage_degraded, t.questions);
    t.out_of_scope_rate = rate(t.out_of_scope, t.questions);
    t.unrelated_rate = rate(t.unrelated, t.questions);
    t.none_found_rate = rate(none_found, t.answered);
    t.not_relevant_rate = rate(not_relevant, t.answered);
    t.flagged_rate = rate(flagged, t.questions);
    Ok(TriageReport {
        since,
        totals: t,
        by_category,
        disagreements,
    })
}

/// One row of `v_feedback_counts`: feedback per day, target, reason and privacy mode.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FeedbackCounts {
    /// The day.
    pub day: DateTime<Utc>,
    /// `excerpt`, `summary` or `query`.
    pub target: String,
    /// The reason, `note` or `flag_query`.
    pub kind: String,
    /// `share` or `count_only`.
    pub privacy_mode: String,
    /// Rows.
    pub rows: i64,
    /// Submissions those rows came in.
    pub submissions: i64,
}

/// Feedback counts since `since` (Spec 06, Privacy modes, `wt report feedback`).
pub async fn feedback(pool: &PgPool, since: DateTime<Utc>) -> Result<Vec<FeedbackCounts>, DbError> {
    let rows = sqlx::query!(
        r#"select day as "day!", target as "target!", kind as "kind!",
                  privacy_mode as "privacy_mode!", rows as "rows!", submissions as "submissions!"
           from v_feedback_counts where day >= date_trunc('day', $1::timestamptz)
           order by day, target, kind, privacy_mode"#,
        since
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| FeedbackCounts {
            day: r.day,
            target: r.target,
            kind: r.kind,
            privacy_mode: r.privacy_mode,
            rows: r.rows,
            submissions: r.submissions,
        })
        .collect())
}

/// A tester's note with what it was about, never with the question.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FeedbackNote {
    /// When it was sent.
    pub created_at: DateTime<Utc>,
    /// `excerpt` or `summary`.
    pub target: String,
    /// The reasons sent with it.
    pub reasons: Vec<String>,
    /// The note, with personal details removed.
    pub note: String,
}

/// Notes since `since`, through `feedback_notes()`.
pub async fn feedback_notes(
    pool: &PgPool,
    since: DateTime<Utc>,
) -> Result<Vec<FeedbackNote>, DbError> {
    let rows = sqlx::query!(
        r#"select created_at as "created_at!", target as "target!", reasons as "reasons!",
                  note as "note!"
           from feedback_notes($1)"#,
        since
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| FeedbackNote {
            created_at: r.created_at,
            target: r.target,
            reasons: r.reasons,
            note: r.note,
        })
        .collect())
}

/// One question in the beta report's period, without its text or its tester's id, from
/// `beta_questions()`.
#[derive(Debug, Clone, PartialEq)]
pub struct BetaQuestion {
    /// The tester, numbered within one report only.
    pub tester: i32,
    /// An answer was stored, whether or not the purge has removed it since.
    pub answered: bool,
    /// The answer's model cost, when recorded.
    pub llm_cost_usd: Option<f64>,
    /// Milliseconds to the first progress event, for a streamed ask.
    pub first_progress_ms: Option<i32>,
    /// Milliseconds to the stored answer behind the page.
    pub page_ms: Option<i32>,
    /// Dana link clicks on the answer while it is kept.
    pub link_clicks: i32,
    /// How the answer's summary settled (Spec 11 PB2.4); none once purged or before statuses.
    pub summary_status: Option<String>,
}

/// What `wt report beta` reads, before it is summed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BetaInputs {
    /// The period's questions.
    pub questions: Vec<BetaQuestion>,
    /// Testers who asked in two or more distinct weeks since the beta began.
    pub returning_testers: i64,
    /// Dana link clicks in the period, those on purged answers included.
    pub link_clicks: i64,
    /// Feedback submissions in the period.
    pub feedback_submissions: i64,
    /// Of those, submissions that marked the answer helpful.
    pub feedback_helpful: i64,
    /// Submissions per reason.
    pub feedback_reasons: BTreeMap<String, i64>,
}

/// Testers in the period.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct BetaTesters {
    /// Testers with at least one question in the period.
    pub active: i64,
    /// Testers with questions in two or more distinct weeks since the beta began.
    pub returning: i64,
}

/// Questions in the period.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct BetaQuestions {
    /// Questions asked, count-only and stopped ones included.
    pub asked: i64,
    /// Of those, answered.
    pub answered: i64,
    /// The median of questions per active tester.
    pub median_per_active_tester: f64,
}

/// Feedback in the period.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct BetaFeedback {
    /// Submissions.
    pub submissions: i64,
    /// Submissions that marked the answer helpful.
    pub helpful: i64,
    /// `helpful` over `submissions`.
    pub helpful_share: f64,
    /// Submissions per reason.
    pub reasons: BTreeMap<String, i64>,
}

/// Dana click-through in the period. With a small group, a floor test, not a conversion rate.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct BetaDana {
    /// Link clicks.
    pub link_clicks: i64,
    /// Link clicks over answers.
    pub clicks_per_answer: f64,
    /// Active testers with at least one click on an answer still kept.
    pub testers_with_click: i64,
    /// `testers_with_click` over active testers.
    pub share_of_testers_with_click: f64,
}

/// One summary status's answers.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct StatusCount {
    /// Answers with the status.
    pub answers: i64,
    /// Their share of answers.
    pub share: f64,
}

/// Summaries in the period, by `answer.summary_status` (Spec 11 PB2.4). The leading metric is
/// `shown_share`: at least 90% of answers that tried for a summary should show one.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct BetaSummaries {
    /// Answers that tried for a summary: every status but `not_attempted`, `pending` and
    /// `unrecorded`.
    pub attempted: i64,
    /// `shown` over `attempted`.
    pub shown_share: f64,
    /// Answers per status, as a share of every answer. `unrecorded` is an answer the purge has
    /// removed since, or one stored before statuses.
    pub by_status: BTreeMap<String, StatusCount>,
}

/// Model cost per answered question, over the answers whose cost was recorded.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct BetaCost {
    /// Answers with a recorded cost.
    pub answers: i64,
    /// Mean, in US dollars.
    pub mean_usd: f64,
    /// 95th percentile, in US dollars.
    pub p95_usd: f64,
}

/// Times from the request, in milliseconds.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct BetaSpeed {
    /// Streamed asks timed to their first progress event.
    pub streamed: i64,
    /// Median time to the first progress event.
    pub first_progress_p50_ms: f64,
    /// 95th percentile time to the first progress event.
    pub first_progress_p95_ms: f64,
    /// Answers timed to their page.
    pub pages: i64,
    /// Median time to the page.
    pub page_p50_ms: f64,
    /// 95th percentile time to the page.
    pub page_p95_ms: f64,
}

/// `wt report beta` (Spec 11 PB4.6): totals only, never question text.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BetaReport {
    /// The start of the period.
    pub since: DateTime<Utc>,
    /// Active and returning testers.
    pub testers: BetaTesters,
    /// Questions.
    pub questions: BetaQuestions,
    /// Feedback.
    pub feedback: BetaFeedback,
    /// Dana click-through.
    pub dana: BetaDana,
    /// The share of answers showing a summary, by status.
    pub summaries: BetaSummaries,
    /// Model cost per question.
    pub cost: BetaCost,
    /// Time to the first progress event and to the page.
    pub speed: BetaSpeed,
}

/// The `p` percentile of `values` by nearest rank, zero when there are none.
fn percentile(values: &[f64], p: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    // Nearest rank: the smallest value with at least `p` of the values at or below it.
    let rank = (p * n as f64).ceil() as usize;
    sorted
        .get(rank.clamp(1, n.max(1)) - 1)
        .copied()
        .unwrap_or(0.0)
}

/// The median of `values`, the mean of the middle two when their number is even.
fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    let mid = |i: usize| sorted.get(i).copied().unwrap_or(0.0);
    if n % 2 == 1 {
        mid(n / 2)
    } else if n == 0 {
        0.0
    } else {
        (mid(n / 2 - 1) + mid(n / 2)) / 2.0
    }
}

/// The mean of `values`, zero when there are none.
fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

/// A length as a count.
fn count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

impl BetaReport {
    /// Sums the period's rows.
    pub fn from_inputs(since: DateTime<Utc>, inputs: &BetaInputs) -> Self {
        // Questions and link clicks per tester.
        let mut per_tester: BTreeMap<i32, (i64, i64)> = BTreeMap::new();
        for q in &inputs.questions {
            let entry = per_tester.entry(q.tester).or_default();
            entry.0 += 1;
            entry.1 += i64::from(q.link_clicks);
        }
        let active = count(per_tester.len());
        let per_active: Vec<f64> = per_tester.values().map(|(n, _)| *n as f64).collect();
        let with_click = count(per_tester.values().filter(|(_, c)| *c > 0).count());
        let answered: Vec<&BetaQuestion> = inputs.questions.iter().filter(|q| q.answered).collect();
        let mut statuses: BTreeMap<String, i64> = BTreeMap::new();
        for q in &answered {
            let status = q.summary_status.as_deref().unwrap_or("unrecorded");
            *statuses.entry(status.to_owned()).or_default() += 1;
        }
        let attempted: i64 = statuses
            .iter()
            .filter(|(s, _)| !matches!(s.as_str(), "not_attempted" | "pending" | "unrecorded"))
            .map(|(_, n)| n)
            .sum();
        let shown = statuses.get("shown").copied().unwrap_or(0);
        let by_status = statuses
            .into_iter()
            .map(|(s, n)| {
                let share = rate(n, count(answered.len()));
                (s, StatusCount { answers: n, share })
            })
            .collect();
        let costs: Vec<f64> = answered.iter().filter_map(|q| q.llm_cost_usd).collect();
        let firsts: Vec<f64> = answered
            .iter()
            .filter_map(|q| q.first_progress_ms.map(f64::from))
            .collect();
        let pages: Vec<f64> = answered
            .iter()
            .filter_map(|q| q.page_ms.map(f64::from))
            .collect();
        Self {
            since,
            testers: BetaTesters {
                active,
                returning: inputs.returning_testers,
            },
            questions: BetaQuestions {
                asked: count(inputs.questions.len()),
                answered: count(answered.len()),
                median_per_active_tester: median(&per_active),
            },
            feedback: BetaFeedback {
                submissions: inputs.feedback_submissions,
                helpful: inputs.feedback_helpful,
                helpful_share: rate(inputs.feedback_helpful, inputs.feedback_submissions),
                reasons: inputs.feedback_reasons.clone(),
            },
            dana: BetaDana {
                link_clicks: inputs.link_clicks,
                clicks_per_answer: rate(inputs.link_clicks, count(answered.len())),
                testers_with_click: with_click,
                share_of_testers_with_click: rate(with_click, active),
            },
            summaries: BetaSummaries {
                attempted,
                shown_share: rate(shown, attempted),
                by_status,
            },
            cost: BetaCost {
                answers: count(costs.len()),
                mean_usd: mean(&costs),
                p95_usd: percentile(&costs, 0.95),
            },
            speed: BetaSpeed {
                streamed: count(firsts.len()),
                first_progress_p50_ms: percentile(&firsts, 0.5),
                first_progress_p95_ms: percentile(&firsts, 0.95),
                pages: count(pages.len()),
                page_p50_ms: percentile(&pages, 0.5),
                page_p95_ms: percentile(&pages, 0.95),
            },
        }
    }
}

/// `wt report beta` since `since`, through the `beta_*` functions `wt_analyst` may run.
pub async fn beta(pool: &PgPool, since: DateTime<Utc>) -> Result<BetaReport, DbError> {
    let questions = sqlx::query!(
        r#"select tester as "tester!", answered as "answered!", llm_cost_usd,
                  first_progress_ms, page_ms, link_clicks as "link_clicks!", summary_status
           from beta_questions($1)"#,
        since
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| BetaQuestion {
        tester: r.tester,
        answered: r.answered,
        llm_cost_usd: r.llm_cost_usd,
        first_progress_ms: r.first_progress_ms,
        page_ms: r.page_ms,
        link_clicks: r.link_clicks,
        summary_status: r.summary_status,
    })
    .collect();
    let returning_testers = sqlx::query_scalar!(r#"select beta_returning_testers() as "n!""#)
        .fetch_one(pool)
        .await?;
    let link_clicks = sqlx::query_scalar!(r#"select beta_link_clicks($1) as "n!""#, since)
        .fetch_one(pool)
        .await?;
    let feedback = sqlx::query!(
        r#"select submissions as "submissions!", helpful as "helpful!" from beta_feedback($1)"#,
        since
    )
    .fetch_one(pool)
    .await?;
    let feedback_reasons = sqlx::query!(
        r#"select kind as "kind!", submissions as "submissions!"
           from beta_feedback_reasons($1)"#,
        since
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| (r.kind, r.submissions))
    .collect();
    Ok(BetaReport::from_inputs(
        since,
        &BetaInputs {
            questions,
            returning_testers,
            link_clicks,
            feedback_submissions: feedback.submissions,
            feedback_helpful: feedback.helpful,
            feedback_reasons,
        },
    ))
}

/// `wt report usage` (Spec 07, Usage report): what each tester did, by label, never question
/// text or a tester's id.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UsageReport {
    /// The start of the period.
    pub since: DateTime<Utc>,
    /// Every tester other than `eval`, active or not.
    pub testers: Vec<UsageTester>,
    /// Page views per method and route template.
    pub routes: Vec<UsageRoute>,
    /// Activity per UTC day.
    pub days: Vec<UsageDay>,
}

/// One tester's activity since the start of the period.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UsageTester {
    /// How the owner knows them.
    pub label: String,
    /// Whether the invite was revoked.
    pub revoked: bool,
    /// When the invite was made.
    pub invited: DateTime<Utc>,
    /// The last request with their session.
    pub last_seen: Option<DateTime<Utc>>,
    /// Whether they ever used the invite.
    pub opened: bool,
    /// Requests with their session.
    pub page_views: i32,
    /// Runs of page views with no gap over 30 minutes.
    pub visits: i32,
    /// UTC days with a page view or a question.
    pub days_active: i32,
    /// The first page view.
    pub first_view: Option<DateTime<Utc>>,
    /// The last page view.
    pub last_view: Option<DateTime<Utc>>,
    /// Questions asked.
    pub questions: i32,
    /// Questions answered.
    pub answered: i32,
    /// Questions they flagged.
    pub flagged: i32,
    /// Feedback submissions.
    pub feedback: i32,
    /// Dana link clicks.
    pub dana_clicks: i32,
}

/// Page views of one route template.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UsageRoute {
    /// The HTTP method.
    pub method: String,
    /// The route template, such as `/a/{answer}`.
    pub route: String,
    /// Requests.
    pub page_views: i32,
    /// Testers who made them.
    pub testers: i32,
    /// Responses with status 500 or above.
    pub errors: i32,
}

/// Activity on one UTC day.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UsageDay {
    /// The day.
    pub day: NaiveDate,
    /// Testers with a page view or a question.
    pub active_testers: i32,
    /// Page views.
    pub page_views: i32,
    /// Questions.
    pub questions: i32,
}

/// `wt report usage` since `since`, through the `usage_*` functions `wt_analyst` may run.
pub async fn usage(pool: &PgPool, since: DateTime<Utc>) -> Result<UsageReport, DbError> {
    let testers = sqlx::query_as!(
        UsageTester,
        r#"select label as "label!", revoked as "revoked!", invited as "invited!", last_seen,
                  opened as "opened!", page_views as "page_views!", visits as "visits!",
                  days_active as "days_active!", first_view, last_view,
                  questions as "questions!", answered as "answered!", flagged as "flagged!",
                  feedback as "feedback!", dana_clicks as "dana_clicks!"
           from usage_testers($1)"#,
        since
    )
    .fetch_all(pool)
    .await?;
    let routes = sqlx::query_as!(
        UsageRoute,
        r#"select method as "method!", route as "route!", page_views as "page_views!",
                  testers as "testers!", errors as "errors!"
           from usage_routes($1)"#,
        since
    )
    .fetch_all(pool)
    .await?;
    let days = sqlx::query_as!(
        UsageDay,
        r#"select day as "day!", active_testers as "active_testers!",
                  page_views as "page_views!", questions as "questions!"
           from usage_days($1)"#,
        since
    )
    .fetch_all(pool)
    .await?;
    Ok(UsageReport {
        since,
        testers,
        routes,
        days,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_the_nearest_rank() {
        let v: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(percentile(&v, 0.95), 19.0);
        assert_eq!(percentile(&v, 0.5), 10.0);
        assert_eq!(percentile(&[7.0], 0.95), 7.0);
        assert_eq!(percentile(&[], 0.95), 0.0);
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), 2.5);
        assert_eq!(median(&[]), 0.0);
    }
}
