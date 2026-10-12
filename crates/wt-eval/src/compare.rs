//! `wt eval compare` (Spec 07, command 6): per-question changes between two runs, matched by
//! question text so runs over different versions of a set still line up.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde::Serialize;
use sqlx::PgPool;
use sqlx::types::Uuid;

use crate::EvalError;
use crate::report::{self, Report};

/// One question in both runs.
#[derive(Debug, Clone, Serialize)]
pub struct Change {
    /// The question.
    pub text: String,
    /// Its id in run A.
    pub a_id: String,
    /// Its id in run B.
    pub b_id: String,
    /// Teachers in A and B.
    pub teachers: (usize, usize),
    /// Passages in A and B.
    pub passages: (usize, usize),
    /// Passages in both, over passages in either.
    pub overlap: f64,
    /// Pass status in A and B, `None` where unrated.
    pub passes: (Option<bool>, Option<bool>),
    /// Whole-path latency in A and B.
    pub total_ms: (u64, u64),
}

/// The comparison.
#[derive(Debug, Clone, Serialize)]
pub struct Comparison {
    /// Run A.
    pub a: Uuid,
    /// Run B.
    pub b: Uuid,
    /// Questions in both runs.
    pub changes: Vec<Change>,
    /// Questions only in A.
    pub only_a: Vec<String>,
    /// Questions only in B.
    pub only_b: Vec<String>,
    /// Approved questions passing in A and B.
    pub passing: (usize, usize),
    /// Median whole-path latency in A and B.
    pub p50_total_ms: (u64, u64),
}

/// Compares two runs.
pub async fn compare(pool: &PgPool, a: Uuid, b: Uuid) -> Result<Comparison, EvalError> {
    let ra = report::build(pool, a, report::QUESTION_PASS_RATIO).await?;
    let rb = report::build(pool, b, report::QUESTION_PASS_RATIO).await?;
    let pa = passage_sets(pool, a).await?;
    let pb = passage_sets(pool, b).await?;
    let mut changes = Vec::new();
    let mut only_a = Vec::new();
    for qa in &ra.questions {
        let Some(qb) = rb.questions.iter().find(|q| q.text == qa.text) else {
            only_a.push(qa.text.clone());
            continue;
        };
        let sa = pa.iter().find(|(t, _)| *t == qa.text).map(|(_, s)| s);
        let sb = pb.iter().find(|(t, _)| *t == qb.text).map(|(_, s)| s);
        let overlap = match (sa, sb) {
            (Some(x), Some(y)) if !x.is_empty() || !y.is_empty() => {
                #[allow(clippy::cast_precision_loss)]
                let r = x.intersection(y).count() as f64 / x.union(y).count() as f64;
                r
            }
            _ => 1.0,
        };
        changes.push(Change {
            text: qa.text.clone(),
            a_id: qa.id.clone(),
            b_id: qb.id.clone(),
            teachers: (qa.teachers, qb.teachers),
            passages: (qa.passages, qb.passages),
            overlap,
            passes: (qa.passes, qb.passes),
            total_ms: (qa.total_ms, qb.total_ms),
        });
    }
    let only_b = rb
        .questions
        .iter()
        .filter(|q| !ra.questions.iter().any(|x| x.text == q.text))
        .map(|q| q.text.clone())
        .collect();
    Ok(Comparison {
        a,
        b,
        changes,
        only_a,
        only_b,
        passing: (ra.questions_passing, rb.questions_passing),
        p50_total_ms: (total_p50(&ra), total_p50(&rb)),
    })
}

fn total_p50(r: &Report) -> u64 {
    r.latency
        .iter()
        .find(|(n, _)| n == "total")
        .map_or(0, |(_, p)| p.p50)
}

async fn passage_sets(
    pool: &PgPool,
    run: Uuid,
) -> Result<Vec<(String, BTreeSet<Uuid>)>, EvalError> {
    Ok(crate::run::results(pool, run)
        .await?
        .into_iter()
        .map(|a| {
            let set = a.payload.passage_ids.iter().map(|p| p.as_uuid()).collect();
            (a.text, set)
        })
        .collect())
}

/// The comparison as plain text.
pub fn render(c: &Comparison) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "A {}  B {}", c.a, c.b);
    let _ = writeln!(
        s,
        "Approved questions passing: {} -> {}; median latency {} -> {} ms",
        c.passing.0, c.passing.1, c.p50_total_ms.0, c.p50_total_ms.1
    );
    let mark = |p: Option<bool>| match p {
        Some(true) => "pass",
        Some(false) => "fail",
        None => "-",
    };
    for ch in &c.changes {
        let _ = writeln!(
            s,
            "  {}/{} teachers {}->{} passages {}->{} overlap {:.2} {}->{} {}->{} ms",
            ch.a_id,
            ch.b_id,
            ch.teachers.0,
            ch.teachers.1,
            ch.passages.0,
            ch.passages.1,
            ch.overlap,
            mark(ch.passes.0),
            mark(ch.passes.1),
            ch.total_ms.0,
            ch.total_ms.1
        );
    }
    for t in &c.only_a {
        let _ = writeln!(s, "  only in A: {t}");
    }
    for t in &c.only_b {
        let _ = writeln!(s, "  only in B: {t}");
    }
    s
}
