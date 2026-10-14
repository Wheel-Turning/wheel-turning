//! `wt eval` (Spec 07, Evaluation harness): load a question set, run it through the ask path,
//! write the reviewer's page, import their ratings, report and compare.

use std::path::{Path, PathBuf};
use std::process::Command as Process;

use anyhow::Context;
use clap::Subcommand;
use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::Config;
use wt_core::config::LlmStage;
use wt_eval::SetRef;
use wt_eval::report::QUESTION_PASS_RATIO;
use wt_eval::run::RunConfig;

use crate::commands::print_json;

/// `wt eval`'s subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum EvalCommand {
    /// Load a question file as a set. A loaded set never changes; load a change as a new
    /// version.
    Load {
        /// The TOML question file, such as eval/questions_v1.toml.
        #[arg(long)]
        file: PathBuf,
        /// The set's name, such as practitioner.
        #[arg(long)]
        name: String,
        /// The set's version.
        #[arg(long)]
        version: i32,
    },
    /// Ask every question of a set through the same path as the answer page, as the reserved
    /// tester `eval`, with stage timings. Uses the models and config/ files the server uses.
    Run {
        /// The set, as name@version.
        #[arg(long)]
        set: SetRef,
        /// Ask only the first N questions.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Write the self-contained review page for the teacher reviewer.
    ExportReview {
        /// The run.
        #[arg(long)]
        run: Uuid,
        /// Where to write it. It quotes transcripts, so keep it out of git.
        #[arg(long)]
        out: PathBuf,
    },
    /// Load the ratings file the review page saved into eval_rating and claim_audit.
    ImportRatings {
        /// The run.
        #[arg(long)]
        run: Uuid,
        /// The saved ratings JSON.
        #[arg(long)]
        file: PathBuf,
        /// The reviewer's name, as recorded.
        #[arg(long)]
        reviewer: String,
    },
    /// Print the run's metrics and the PRD verdicts.
    Report {
        /// The run.
        #[arg(long)]
        run: Uuid,
        /// `question_pass_ratio`.
        #[arg(long, default_value_t = QUESTION_PASS_RATIO)]
        pass_ratio: f64,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Score where the inappropriate-question gates send each question of a gate file, such
    /// as eval/gate_questions_v1.toml: the rules, the embedding check and the triage gate, as
    /// the answer page applies them. Retrieves nothing and stores nothing but the spend
    /// ledger's count of the gate's calls.
    Gate {
        /// The gate question file.
        #[arg(long)]
        file: PathBuf,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Per-question changes between two runs.
    Compare {
        /// The earlier run.
        #[arg(long)]
        a: Uuid,
        /// The later run.
        #[arg(long)]
        b: Uuid,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
}

/// Dispatches `wt eval`.
pub(crate) async fn eval(
    pool: &PgPool,
    config: &Config,
    command: EvalCommand,
) -> anyhow::Result<()> {
    match command {
        EvalCommand::Load {
            file,
            name,
            version,
        } => {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let questions = wt_eval::questions::parse(&text)?;
            let set = SetRef { name, version };
            let (id, created) = wt_eval::questions::load(pool, &set, &questions).await?;
            let drafts = questions.iter().filter(|q| q.is_draft()).count();
            print_json(&serde_json::json!({
                "set": set.to_string(), "id": id, "created": created,
                "questions": questions.len(), "drafts": drafts,
            }))?;
        }
        EvalCommand::Run { set, limit } => {
            let site = wt_api::site::load(config, pool)
                .await
                .context("loading the models")?;
            let run_config = run_config(config, &site)?;
            let run = wt_eval::run::run(pool, &site, &set, &run_config, limit, |p| {
                let o = &p.outcome;
                eprintln!(
                    "{} {} teachers, {} passages, {} themes{}{} in {} ms",
                    p.label,
                    o.teacher_count,
                    o.passage_count,
                    o.theme_count,
                    o.notice
                        .as_deref()
                        .map(|n| format!(", {n}"))
                        .unwrap_or_default(),
                    if o.synthesis_failed {
                        ", synthesis failed"
                    } else {
                        ""
                    },
                    p.timings.total_ms
                );
            })
            .await?;
            println!("{run}");
        }
        EvalCommand::ExportReview { run, out } => {
            let lead_in = retrieval_config()?.answer.link_lead_in_ms;
            let html = wt_eval::review::export(pool, run, lead_in).await?;
            if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&out, html).with_context(|| format!("writing {}", out.display()))?;
            eprintln!(
                "Wrote {}. It quotes transcripts: send it privately and keep it out of git.",
                out.display()
            );
        }
        EvalCommand::ImportRatings {
            run,
            file,
            reviewer,
        } => {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let ratings: wt_eval::ratings::RatingsFile =
                serde_json::from_str(&text).context("parsing the ratings file")?;
            let done = wt_eval::ratings::import(pool, run, &ratings, &reviewer).await?;
            print_json(&done)?;
        }
        EvalCommand::Report {
            run,
            pass_ratio,
            json,
        } => {
            anyhow::ensure!(
                (0.0..=1.0).contains(&pass_ratio),
                "--pass-ratio is a share between 0 and 1"
            );
            let report = wt_eval::report::build(pool, run, pass_ratio).await?;
            if json {
                print_json(&report)?;
            } else {
                print!("{}", wt_eval::report::render(&report));
            }
        }
        EvalCommand::Gate { file, json } => {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let questions = wt_eval::gate::parse(&text)?;
            let site = wt_api::site::load(config, pool)
                .await
                .context("loading the models and config/ files the server uses")?;
            // The gate as deployed, with the questions marked synthetic, as for the red team.
            let gate = wt_retrieve::sensitive::triage::LlmTriage::synthetic(site.llm.clone());
            let results = wt_eval::gate::run(
                &site.sensitivity,
                site.embedder.as_ref(),
                Some(&gate),
                questions,
                |r| {
                    eprintln!(
                        "{} {} {}",
                        r.label(),
                        if r.route_ok() { "ok  " } else { "MISS" },
                        r.observed.route.as_str()
                    );
                },
            )
            .await?;
            let report = wt_eval::gate::report(&results);
            if json {
                print_json(&report)?;
            } else {
                print!("{}", wt_eval::gate::render(&report));
            }
        }
        EvalCommand::Compare { a, b, json } => {
            let c = wt_eval::compare::compare(pool, a, b).await?;
            if json {
                print_json(&c)?;
            } else {
                print!("{}", wt_eval::compare::render(&c));
            }
        }
    }
    Ok(())
}

/// The retrieval settings the server uses, from config/retrieval.toml, so an eval run ranks as the
/// site does.
fn retrieval_config() -> anyhow::Result<wt_retrieve::RetrievalConfig> {
    let path = wt_api::site::config_dir().join("retrieval.toml");
    wt_retrieve::RetrievalConfig::load(&path).with_context(|| format!("loading {}", path.display()))
}

/// The build, commit, configuration snapshot and model names the run records.
fn run_config(config: &Config, site: &wt_api::Site) -> anyhow::Result<RunConfig> {
    let dir = wt_api::site::config_dir();
    let read = |p: &Path| std::fs::read_to_string(p).ok();
    let retrieval_toml =
        read(&dir.join("retrieval.toml")).context("reading retrieval.toml for the snapshot")?;
    let model = |stage| config.llm_model_for(stage).ok().map(str::to_owned);
    Ok(RunConfig {
        build: env!("CARGO_PKG_VERSION").to_owned(),
        commit: commit(),
        retrieval_toml,
        pipeline_toml: read(&dir.join("pipeline.toml")),
        models: serde_json::json!({
            "embed": config.embed_model,
            "rerank": site.reranker.as_ref().map(|_| config.rerank_model.clone()),
            "synth": model(LlmStage::Synth),
            "verify": model(LlmStage::Verify),
        }),
    })
}

/// The checkout's commit, marked `-dirty` when tracked files have changes.
fn commit() -> String {
    let git = |args: &[&str]| {
        Process::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
    };
    match git(&["rev-parse", "HEAD"]) {
        Some(head) => match git(&["status", "--porcelain", "--untracked-files=no"]) {
            Some(s) if !s.is_empty() => format!("{head}-dirty"),
            _ => head,
        },
        None => "unknown".to_owned(),
    }
}
