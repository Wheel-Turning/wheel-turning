//! `wt eval import-ratings` (Spec 07, command 4): the reviewer's saved JSON into `eval_rating`
//! and `claim_audit`. A passage counts as rated only when both answers are given. Importing a
//! reviewer's file again replaces their earlier import for the run, so a corrected file can be
//! loaded over the first.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::types::Uuid;

use crate::EvalError;
use crate::run::{self, RunAnswer};

/// The file the review page saves.
#[derive(Debug, Clone, Deserialize)]
pub struct RatingsFile {
    /// The run the page was written for.
    pub run: String,
    /// The name the reviewer typed.
    #[serde(default)]
    pub reviewer: String,
    /// When they saved it.
    #[serde(default)]
    pub rated_on: String,
    /// One entry per passage shown.
    #[serde(default)]
    pub ratings: Vec<PassageRating>,
    /// One entry per summary line shown.
    #[serde(default)]
    pub claims: Vec<ClaimRating>,
}

/// One passage's rating.
#[derive(Debug, Clone, Deserialize)]
pub struct PassageRating {
    /// `q01` and so on.
    pub question_id: String,
    /// The passage.
    pub passage_id: String,
    /// Faithful, or `None` if unanswered.
    pub faithful: Option<bool>,
    /// Relevant, or `None` if unanswered.
    pub relevant: Option<bool>,
    /// The reviewer's note.
    #[serde(default)]
    pub note: String,
}

/// One summary line's verdict.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaimRating {
    /// `q01` and so on.
    pub question_id: String,
    /// The answer holding the theme.
    pub answer_id: String,
    /// The theme's index in the answer.
    pub theme: usize,
    /// `supported`, `drifting` or `invented`, or `None` if unanswered.
    pub verdict: Option<String>,
    /// The reviewer's note.
    #[serde(default)]
    pub note: String,
}

/// What an import wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Imported {
    /// Passage ratings with both answers, written to `eval_rating`.
    pub ratings: usize,
    /// Passages left unrated, not written.
    pub unrated: usize,
    /// Summary-line verdicts written to `claim_audit`.
    pub claims: usize,
    /// Summary lines left without a verdict.
    pub claims_unrated: usize,
}

const VERDICTS: [&str; 3] = ["supported", "drifting", "invented"];

/// Imports a ratings file for `run` as `reviewer`. Every entry must name a question, passage or
/// theme of the run, or nothing is written.
pub async fn import(
    pool: &PgPool,
    run: Uuid,
    file: &RatingsFile,
    reviewer: &str,
) -> Result<Imported, EvalError> {
    let bad = |m: String| EvalError::Ratings(m);
    let reviewer = reviewer.trim();
    if reviewer.is_empty() {
        return Err(bad("a reviewer name is required".into()));
    }
    if file.run.trim() != run.to_string() {
        return Err(bad(format!(
            "the file is for run {}, not {run}",
            file.run.trim()
        )));
    }
    run::find(pool, run).await?;
    let answers = run::results(pool, run).await?;
    let by_label: BTreeMap<String, &RunAnswer> = answers.iter().map(|a| (a.label(), a)).collect();

    let mut out = Imported::default();
    let mut rows = Vec::new();
    for r in &file.ratings {
        let a = by_label
            .get(&r.question_id)
            .ok_or_else(|| bad(format!("{} is not a question of this run", r.question_id)))?;
        let passage: Uuid = r
            .passage_id
            .parse()
            .map_err(|_| bad(format!("{:?} is not a passage id", r.passage_id)))?;
        if !a.payload.passage_ids.iter().any(|p| p.as_uuid() == passage) {
            return Err(bad(format!(
                "passage {passage} was not returned for {}",
                r.question_id
            )));
        }
        match (r.faithful, r.relevant) {
            (Some(f), Some(rel)) => rows.push((a.question_id, passage, f, rel, note(&r.note))),
            _ => out.unrated += 1,
        }
    }
    let mut claims = Vec::new();
    for c in &file.claims {
        let a = by_label
            .get(&c.question_id)
            .ok_or_else(|| bad(format!("{} is not a question of this run", c.question_id)))?;
        if c.answer_id.trim() != a.answer_id.to_string() {
            return Err(bad(format!(
                "{} does not hold answer {}",
                c.question_id, c.answer_id
            )));
        }
        // The claim text comes from the stored answer, never from the file.
        let lines = wt_core::answer::claims(&a.payload.themes, a.payload.summary.as_ref());
        let theme = lines.get(c.theme).ok_or_else(|| {
            bad(format!(
                "{} has no summary line {}",
                c.question_id,
                c.theme + 1
            ))
        })?;
        match c.verdict.as_deref() {
            Some(v) if VERDICTS.contains(&v) => {
                claims.push((a.answer_id, theme.description.clone(), v, note(&c.note)));
            }
            Some(v) => return Err(bad(format!("{v:?} is not a claim verdict"))),
            None => out.claims_unrated += 1,
        }
    }

    let mut tx = pool.begin().await?;
    sqlx::query!(
        "delete from eval_rating where run_id = $1 and reviewer = $2",
        run,
        reviewer
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "delete from claim_audit where run_id = $1 and reviewer = $2",
        run,
        reviewer
    )
    .execute(&mut *tx)
    .await?;
    for (question, passage, faithful, relevant, note) in &rows {
        sqlx::query!(
            "insert into eval_rating (run_id, question_id, passage_id, reviewer, faithful,
                                      relevant, note)
             values ($1, $2, $3, $4, $5, $6, $7)",
            run,
            question,
            passage,
            reviewer,
            faithful,
            relevant,
            note.as_deref()
        )
        .execute(&mut *tx)
        .await?;
    }
    for (answer, claim, verdict, note) in &claims {
        sqlx::query!(
            "insert into claim_audit (run_id, answer_id, claim_text, verdict, reviewer, note)
             values ($1, $2, $3, $4, $5, $6)",
            run,
            answer,
            claim,
            verdict,
            reviewer,
            note.as_deref()
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    out.ratings = rows.len();
    out.claims = claims.len();
    Ok(out)
}

fn note(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_owned())
}
