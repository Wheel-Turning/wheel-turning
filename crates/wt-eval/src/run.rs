//! `wt eval run` (Spec 07, command 2): every question of a set through the ask path, as the
//! reserved tester `eval`, with the run's build, commit, configuration and model names.

use serde::Serialize;
use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_api::Site;
use wt_api::ask::{Asked, StageTimings};
use wt_core::ids::TesterId;
use wt_db::tester::EVAL_LABEL;
use wt_retrieve::Mode;

use crate::questions::{self, SetRef};
use crate::{EvalError, question_label};

/// What identifies the build and configuration a run used. The caller fills it, since only the
/// binary knows its commit and which files it read.
#[derive(Debug, Clone, Serialize)]
pub struct RunConfig {
    /// The build identifier, such as the crate version.
    pub build: String,
    /// The git commit, with `-dirty` when the tree had changes.
    pub commit: String,
    /// `config/retrieval.toml` as read.
    pub retrieval_toml: String,
    /// `config/pipeline.toml` as read, if present.
    pub pipeline_toml: Option<String>,
    /// Model names: embedder, reranker, synthesis and verifier.
    pub models: serde_json::Value,
}

/// How one question came out, kept in `eval_result.outcome`.
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Outcome {
    /// `standard` or `named_teacher`.
    pub mode: String,
    /// Distinct teachers.
    pub teacher_count: usize,
    /// Passages chosen.
    pub passage_count: usize,
    /// Themes shown.
    pub theme_count: usize,
    /// Themes dropped by the filters or the verifier.
    pub themes_dropped: usize,
    /// Synthesis failed or timed out.
    pub synthesis_failed: bool,
    /// The reranker was unavailable.
    pub rerank_degraded: bool,
    /// The notice, if any.
    pub notice: Option<String>,
}

impl From<&Asked> for Outcome {
    fn from(a: &Asked) -> Self {
        Outcome {
            mode: match a.mode {
                Mode::Standard => "standard".into(),
                Mode::NamedTeacher(_) => "named_teacher".into(),
            },
            teacher_count: a.teacher_count,
            passage_count: a.passage_count,
            theme_count: a.theme_count,
            themes_dropped: a.themes_dropped,
            synthesis_failed: a.synthesis_failed,
            rerank_degraded: a.rerank_degraded,
            notice: a.notice.map(str::to_owned),
        }
    }
}

/// One question's line of progress.
#[derive(Debug, Clone)]
pub struct Progress<'a> {
    /// `q01` and so on.
    pub label: String,
    /// The question.
    pub text: &'a str,
    /// How it came out.
    pub outcome: Outcome,
    /// Its timings.
    pub timings: StageTimings,
}

/// The reserved tester evaluation runs ask as. It is created revoked, so no session can ever
/// use it, and the analytics views leave its answers out.
pub async fn eval_tester(pool: &PgPool) -> Result<TesterId, EvalError> {
    if let Some(id) = sqlx::query_scalar!(
        "select id from tester where label = $1 order by created_at limit 1",
        EVAL_LABEL
    )
    .fetch_optional(pool)
    .await?
    {
        return Ok(TesterId::from(id));
    }
    // A hash of no token: nobody holds a token that hashes to random bytes.
    let hash: Vec<u8> = [
        Uuid::new_v4().as_bytes().as_slice(),
        Uuid::new_v4().as_bytes().as_slice(),
    ]
    .concat();
    let id = sqlx::query_scalar!(
        "insert into tester (label, token_hash, revoked_at) values ($1, $2, now()) returning id",
        EVAL_LABEL,
        hash
    )
    .fetch_one(pool)
    .await?;
    Ok(TesterId::from(id))
}

/// Runs every question of `set` (or the first `limit`) and returns the run's id. A question
/// that fails stops the run, which stays unfinished in the report.
pub async fn run(
    pool: &PgPool,
    site: &Site,
    set: &SetRef,
    config: &RunConfig,
    limit: Option<usize>,
    mut progress: impl FnMut(&Progress<'_>),
) -> Result<Uuid, EvalError> {
    let set_id = questions::find(pool, set).await?;
    let mut conn = pool.acquire().await?;
    let mut qs = questions::questions_of(&mut conn, set_id).await?;
    drop(conn);
    if let Some(n) = limit {
        qs.truncate(n);
    }
    let tester = eval_tester(pool).await?;
    let config_json = serde_json::to_value(config).unwrap_or_default();
    let run = sqlx::query_scalar!(
        "insert into eval_run (set_id, build, config) values ($1, $2, $3) returning id",
        set_id,
        config.build,
        config_json
    )
    .fetch_one(pool)
    .await?;
    tracing::info!(%run, set = %set, questions = qs.len(), "evaluation run started");

    for q in &qs {
        let text = wt_retrieve::query::normalize(&q.question.text)
            .map_err(|e| EvalError::Questions(format!("{}: {e}", question_label(q.ordinal))))?;
        let asked = wt_api::ask::answer(pool, site, tester, &text)
            .await
            .map_err(|source| EvalError::Ask {
                ordinal: q.ordinal,
                source,
            })?;
        let outcome = Outcome::from(&asked);
        sqlx::query!(
            "insert into eval_result (run_id, question_id, answer_id, timings, outcome)
             values ($1, $2, $3, $4, $5)",
            run,
            q.id,
            asked.answer_id.as_uuid(),
            serde_json::to_value(asked.timings).unwrap_or_default(),
            serde_json::to_value(&outcome).unwrap_or_default()
        )
        .execute(pool)
        .await?;
        progress(&Progress {
            label: question_label(q.ordinal),
            text: &q.question.text,
            outcome,
            timings: asked.timings,
        });
    }

    sqlx::query!("update eval_run set finished_at = now() where id = $1", run)
        .execute(pool)
        .await?;
    Ok(run)
}

/// A run as stored.
#[derive(Debug, Clone)]
pub struct StoredRun {
    /// The run.
    pub id: Uuid,
    /// Its set.
    pub set: SetRef,
    /// The build identifier.
    pub build: String,
    /// Build, commit, configuration and models.
    pub config: serde_json::Value,
    /// When it started.
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// When it finished; `None` if it stopped partway.
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Questions in the set.
    pub set_size: usize,
}

/// One question's answer in a run.
#[derive(Debug, Clone)]
pub struct RunAnswer {
    /// The question's row.
    pub question_id: Uuid,
    /// Its place in the set.
    pub ordinal: i32,
    /// The question.
    pub text: String,
    /// Its tags.
    pub tags: Vec<String>,
    /// The stored answer.
    pub answer_id: Uuid,
    /// What the answer holds.
    pub payload: wt_db::answer::Payload,
    /// Stage timings.
    pub timings: StageTimings,
    /// How it came out.
    pub outcome: Option<Outcome>,
    /// The named teacher's display name, in named mode.
    pub named_teacher: Option<String>,
    /// From `query_log`.
    pub thin_coverage: bool,
}

impl RunAnswer {
    /// `q01` and so on.
    pub fn label(&self) -> String {
        question_label(self.ordinal)
    }

    /// Whether the owner has yet to approve the question.
    pub fn is_draft(&self) -> bool {
        questions::is_draft(&self.tags)
    }
}

/// The run's row.
pub async fn find(pool: &PgPool, run: Uuid) -> Result<StoredRun, EvalError> {
    let r = sqlx::query!(
        r#"select r.id, s.name, s.version, r.build, r.config, r.started_at, r.finished_at,
                  (select count(*) from eval_question q where q.set_id = s.id) as "set_size!"
           from eval_run r join eval_set s on s.id = r.set_id where r.id = $1"#,
        run
    )
    .fetch_optional(pool)
    .await?
    .ok_or(EvalError::UnknownRun(run))?;
    Ok(StoredRun {
        id: r.id,
        set: SetRef {
            name: r.name,
            version: r.version,
        },
        build: r.build,
        config: r.config,
        started_at: r.started_at,
        finished_at: r.finished_at,
        set_size: usize::try_from(r.set_size).unwrap_or(0),
    })
}

/// Every answer of a run, in question order.
pub async fn results(pool: &PgPool, run: Uuid) -> Result<Vec<RunAnswer>, EvalError> {
    let rows = sqlx::query!(
        r#"select q.id as question_id, q.ordinal, q.text, q.tags, r.answer_id, a.payload,
                  r.timings, r.outcome, l.thin_coverage, te.display_name as "named_teacher?"
           from eval_result r
           join eval_question q on q.id = r.question_id
           join answer a on a.id = r.answer_id
           join query_log l on l.id = a.query_log_id
           left join teacher te on te.id = l.named_teacher_id
           where r.run_id = $1
           order by q.ordinal"#,
        run
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|r| {
            Ok(RunAnswer {
                question_id: r.question_id,
                ordinal: r.ordinal,
                text: r.text,
                tags: r.tags,
                answer_id: r.answer_id,
                payload: serde_json::from_value(r.payload)
                    .map_err(|e| EvalError::Payload(r.answer_id, e))?,
                timings: serde_json::from_value(r.timings).unwrap_or_default(),
                outcome: serde_json::from_value(r.outcome).ok(),
                named_teacher: r.named_teacher,
                thin_coverage: r.thin_coverage,
            })
        })
        .collect()
}
