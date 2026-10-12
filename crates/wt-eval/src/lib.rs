//! The evaluation harness (Spec 07, Evaluation harness).
//!
//! A question set is loaded from `eval/*.toml` into `eval_set` and `eval_question`, and is
//! immutable once loaded. A run asks every question through [`wt_api::ask::answer`], the same
//! path the `/ask` route takes, as the reserved tester label `eval`, and records each answer with
//! its stage timings. The reviewer's file comes from [`review::export`], their ratings go back in
//! through [`ratings::import`], and [`report::build`] prints the PRD's pass and fail verdicts.

pub mod compare;
pub mod questions;
pub mod ratings;
pub mod report;
pub mod review;
pub mod run;

use sqlx::types::Uuid;

pub use questions::{Question, SetRef};

/// Why an evaluation command failed.
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    /// The question file could not be parsed.
    #[error("question file: {0}")]
    Questions(String),
    /// A set with this name and version holds different questions.
    #[error("{0} is already loaded with different questions; load the change as a new version")]
    SetChanged(SetRef),
    /// No set with this name and version.
    #[error("no question set {0}; load it with wt eval load")]
    UnknownSet(SetRef),
    /// No run with this id.
    #[error("no evaluation run {0}")]
    UnknownRun(Uuid),
    /// A question could not be answered.
    #[error("question {ordinal}: {source}")]
    Ask {
        /// The question's position in the set.
        ordinal: i32,
        /// What went wrong.
        source: wt_api::ask::AskError,
    },
    /// The ratings file does not fit the run.
    #[error("ratings file: {0}")]
    Ratings(String),
    /// A stored answer could not be read.
    #[error("answer {0} has a payload this build cannot read: {1}")]
    Payload(Uuid, serde_json::Error),
    /// The database refused.
    #[error(transparent)]
    Db(#[from] wt_db::DbError),
}

impl From<sqlx::Error> for EvalError {
    fn from(e: sqlx::Error) -> Self {
        EvalError::Db(wt_db::DbError::Sqlx(e))
    }
}

/// The reviewer-facing id of a question: `q01`, `q02`, ... by its place in the set.
pub fn question_label(ordinal: i32) -> String {
    format!("q{ordinal:02}")
}
