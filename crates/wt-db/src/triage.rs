//! The triage gate's tally (Spec 05, Sensitive questions): what each layer said, counted per
//! day with no link to a question, so `wt sensitive report` can show where the gate and the
//! rules or exemplars disagree without anyone reading question text.

use sqlx::PgPool;

use crate::DbError;

/// One question's layer verdicts, as the strings the tally keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tally<'a> {
    /// The rules' level.
    pub rules: &'a str,
    /// The exemplars' level, `down` or `skipped`.
    pub embedding: &'a str,
    /// The gate's risk, or `unanswered`.
    pub risk: &'a str,
    /// The level that risk maps to, or `unanswered`.
    pub gate_level: &'a str,
    /// The gate's relevance, or `unanswered`.
    pub relevance: &'a str,
    /// The gate's confidence, or `unanswered`.
    pub confidence: &'a str,
    /// The level the question got.
    pub level: &'a str,
}

/// Adds one to today's count for `t`.
pub async fn count(pool: &PgPool, t: &Tally<'_>) -> Result<(), DbError> {
    sqlx::query!(
        "insert into triage_tally (day, rules, embedding, risk, gate_level, relevance, confidence,
                                   level, n)
         values (current_date, $1, $2, $3, $4, $5, $6, $7, 1)
         on conflict (day, rules, embedding, risk, gate_level, relevance, confidence, level)
         do update set n = triage_tally.n + 1",
        t.rules,
        t.embedding,
        t.risk,
        t.gate_level,
        t.relevance,
        t.confidence,
        t.level
    )
    .execute(pool)
    .await?;
    Ok(())
}
