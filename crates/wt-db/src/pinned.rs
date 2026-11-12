//! Pinned answers (Spec 10, D3; owner's decision of November 12, 2026). The owner pins a stored
//! answer to a question they write themselves. A tester who asks that question gets a new
//! answer of their own that reuses the pinned passages, excerpts and summary. The pinned
//! question is the owner's text, never a tester's: [`pin`] takes it from the owner, and
//! [`source`] checks that it is the question the answer was written for.

use sqlx::PgPool;
use sqlx::types::Uuid;
use sqlx::types::chrono::{DateTime, Utc};
use wt_core::ids::AnswerId;

use crate::DbError;
use crate::answer::Payload;

/// The key a question is pinned and looked up under: lowercased, apostrophes dropped and every
/// other run of characters that are not letters or digits turned into one space, so "What is
/// zazen?" and "what is zazen" match.
pub fn key(question: &str) -> String {
    let mut out = String::with_capacity(question.len());
    let mut space = true;
    for c in question.chars() {
        if matches!(c, '\'' | '\u{2019}' | '\u{2018}' | '`') {
            continue;
        }
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            space = false;
        } else if !space {
            out.push(' ');
            space = true;
        }
    }
    out.trim_end().to_owned()
}

/// A pinned answer as the ask path reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct Pinned {
    /// The question as the owner wrote it.
    pub question_text: String,
    /// The pinned payload: passages, excerpt spans, notice and summary.
    pub payload: Payload,
}

/// One row of `wt answer pins`.
#[derive(Debug, Clone, PartialEq)]
pub struct PinRow {
    /// The question as the owner wrote it.
    pub question_text: String,
    /// The answer it was copied from.
    pub source_answer_id: Option<AnswerId>,
    /// Passages it holds.
    pub passages: usize,
    /// Summary sentences it holds.
    pub sentences: usize,
    /// Who pinned it.
    pub pinned_by: String,
    /// When.
    pub pinned_at: DateTime<Utc>,
}

/// The pinned answer for `question`, if any. A payload that no longer reads is treated as no
/// pin, so the question runs normally.
pub async fn find(pool: &PgPool, question: &str) -> Result<Option<Pinned>, DbError> {
    let k = key(question);
    if k.is_empty() {
        return Ok(None);
    }
    let row = sqlx::query!(
        "select question_text, payload from pinned_answer where question_key = $1",
        k
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| {
        serde_json::from_value::<Payload>(r.payload)
            .ok()
            .map(|payload| Pinned {
                question_text: r.question_text,
                payload,
            })
    }))
}

/// Why an answer cannot be pinned.
#[derive(Debug, thiserror::Error)]
pub enum PinError {
    /// No such answer.
    #[error("no answer {0}")]
    NoAnswer(AnswerId),
    /// The answer's question text was not kept, so the pin cannot be checked against it.
    #[error("answer {0} no longer holds its question text, so the pin cannot be checked")]
    NoText(AnswerId),
    /// The owner's question is not the one the answer was written for.
    #[error("answer {answer} was written for a different question: {asked:?}")]
    Mismatch {
        /// The answer.
        answer: AnswerId,
        /// Its question.
        asked: String,
    },
    /// Only a standard answer with a summary, from an ordinary question, can be pinned.
    #[error("answer {0} cannot be pinned: {1}")]
    Unsuitable(AnswerId, &'static str),
    /// The payload did not read or write.
    #[error("answer {0} has a payload that does not read: {1}")]
    BadPayload(AnswerId, serde_json::Error),
    /// The database failed.
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<sqlx::Error> for PinError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(DbError::from(e))
    }
}

/// The payload of `answer`, checked for pinning under `question`: the answer's own question must
/// match it after normalization, and the answer must be an ordinary, standard answer with a
/// summary. Owner role only, since it reads `answer`.
pub async fn source(pool: &PgPool, answer: AnswerId, question: &str) -> Result<Payload, PinError> {
    let row = sqlx::query!(
        r#"select a.payload, q.query_text, q.sensitive_category as "sensitive_category!"
             from answer a join query_log q on q.id = a.query_log_id
            where a.id = $1"#,
        answer.as_uuid()
    )
    .fetch_optional(pool)
    .await?
    .ok_or(PinError::NoAnswer(answer))?;
    let asked = row.query_text.ok_or(PinError::NoText(answer))?;
    if key(&asked) != key(question) {
        return Err(PinError::Mismatch { answer, asked });
    }
    let mut payload: Payload =
        serde_json::from_value(row.payload).map_err(|e| PinError::BadPayload(answer, e))?;
    if row.sensitive_category != "none" {
        return Err(PinError::Unsuitable(
            answer,
            "it was asked as a sensitive question",
        ));
    }
    if payload.mode != "standard" || payload.curated {
        return Err(PinError::Unsuitable(answer, "it is not a standard answer"));
    }
    if payload.summary.is_none() {
        return Err(PinError::Unsuitable(answer, "it has no summary"));
    }
    payload.summary_pending = false;
    Ok(payload)
}

/// Pins `payload` to `question`, replacing any pin on the same key.
pub async fn pin(
    pool: &PgPool,
    question: &str,
    payload: &Payload,
    source: Option<AnswerId>,
    by: &str,
) -> Result<(), PinError> {
    let k = key(question);
    if k.is_empty() {
        return Err(PinError::Unsuitable(
            source.unwrap_or_else(|| AnswerId::from(Uuid::nil())),
            "the question is empty",
        ));
    }
    let value = serde_json::to_value(payload).map_err(|e| {
        PinError::BadPayload(source.unwrap_or_else(|| AnswerId::from(Uuid::nil())), e)
    })?;
    sqlx::query!(
        "insert into pinned_answer (question_key, question_text, payload, source_answer_id, pinned_by)
         values ($1, $2, $3, $4, $5)
         on conflict (question_key) do update
            set question_text = excluded.question_text, payload = excluded.payload,
                source_answer_id = excluded.source_answer_id, pinned_by = excluded.pinned_by,
                pinned_at = now()",
        k,
        question.trim(),
        value,
        source.map(|a| a.as_uuid()),
        by
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Removes the pin on `question`. Returns whether there was one.
pub async fn unpin(pool: &PgPool, question: &str) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "delete from pinned_answer where question_key = $1",
        key(question)
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Every pin, newest first.
pub async fn list(pool: &PgPool) -> Result<Vec<PinRow>, DbError> {
    let rows = sqlx::query!(
        "select question_text, payload, source_answer_id, pinned_by, pinned_at
           from pinned_answer order by pinned_at desc"
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let payload = serde_json::from_value::<Payload>(r.payload).ok();
            PinRow {
                question_text: r.question_text,
                source_answer_id: r.source_answer_id.map(AnswerId::from),
                passages: payload.as_ref().map_or(0, |p| p.passage_ids.len()),
                sentences: payload
                    .as_ref()
                    .and_then(|p| p.summary.as_ref())
                    .map_or(0, |s| s.sentences.len()),
                pinned_by: r.pinned_by,
                pinned_at: r.pinned_at,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::key;

    #[test]
    fn key_ignores_case_punctuation_and_spacing() {
        assert_eq!(
            key("What is the difference between practice and trying to improve myself?"),
            key("  what is the difference between practice   and trying to improve myself ")
        );
        assert_eq!(key("Don’t I"), key("dont i"));
        assert_eq!(key("?!"), "");
    }
}
