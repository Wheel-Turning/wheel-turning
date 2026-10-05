//! `wt ingest discover` and `wt worker` (Spec 03).

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, bail};
use sqlx::PgPool;
use wt_core::Config;
use wt_db::job::{self, Failure, JobKind};
use wt_ingest::config::{ClassifyConfig, SourcesFile};
use wt_ingest::pipeline;
use wt_ingest::vimeo::{VimeoAdapter, VimeoEndpoints};
use wt_ingest::{SourceAdapter, SourcePlatform};

use crate::commands::print_json;

/// How long an idle worker waits before looking for work again.
const IDLE: Duration = Duration::from_secs(5);
/// How often a worker runs the stale-lock reaper.
const REAP_EVERY: Duration = Duration::from_secs(60);

/// Lists each configured source (or the named one) and records new and changed videos, each
/// queued for classification. Prints the counts per source as JSON.
pub async fn discover(
    pool: &PgPool,
    config: &Config,
    config_dir: &Path,
    only: Option<&str>,
) -> anyhow::Result<()> {
    let sources = SourcesFile::load(&config_dir.join("sources.toml"))?;
    if let Some(name) = only {
        sources.get(name)?;
    }
    let mut reports = serde_json::Map::new();
    for source in sources
        .sources
        .iter()
        .filter(|s| only.is_none_or(|n| n == s.name))
    {
        let adapter: Box<dyn SourceAdapter> = match source.platform {
            SourcePlatform::Vimeo => Box::new(VimeoAdapter::new(
                &source.name,
                config.vimeo_access_token.clone(),
                source.talks_page.as_deref(),
                VimeoEndpoints::vimeo()?,
            )?),
            other => {
                if only.is_some() {
                    bail!(
                        "source {} is {other}, which has no discovery adapter yet",
                        source.name
                    );
                }
                tracing::info!(source = %source.name, platform = %other, "no discovery adapter yet; skipped");
                continue;
            }
        };
        let mut conn = pool.acquire().await?;
        let report = pipeline::discover(&mut conn, source, adapter.as_ref(), None)
            .await
            .with_context(|| format!("discovering {}", source.name))?;
        reports.insert(
            source.name.clone(),
            serde_json::json!({
                "listed": report.listed, "new": report.new, "changed": report.changed,
                "unchanged": report.unchanged, "withdrawn": report.withdrawn,
            }),
        );
    }
    print_json(&reports)
}

/// Runs jobs of the named kinds with `concurrency` workers until stopped, or with `drain`
/// until nothing is due. Each worker reaps stale locks once a minute.
pub async fn worker(
    pool: &PgPool,
    config_dir: &Path,
    kinds: &[JobKind],
    concurrency: usize,
    drain: bool,
) -> anyhow::Result<()> {
    if let Some(missing) = kinds.iter().find(|k| **k != JobKind::Classify) {
        bail!("no handler for {missing} jobs yet: this build runs classify");
    }
    let cfg = ClassifyConfig::load(
        &config_dir.join("pipeline.toml"),
        &config_dir.join("series.toml"),
    )?;
    let sources = SourcesFile::load(&config_dir.join("sources.toml"))?;
    let lock_timeout =
        Duration::from_secs(u64::try_from(cfg.settings.job_lock_timeout_s).unwrap_or(7200));
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "worker".to_owned());

    let mut tasks = tokio::task::JoinSet::new();
    for n in 0..concurrency.max(1) {
        let (pool, cfg, sources, kinds) =
            (pool.clone(), cfg.clone(), sources.clone(), kinds.to_vec());
        let name = format!("{host}-{}-{n}", std::process::id());
        tasks.spawn(async move {
            run_worker(&pool, &cfg, &sources, &kinds, &name, lock_timeout, drain).await
        });
    }
    tokio::select! {
        result = async {
            while let Some(done) = tasks.join_next().await {
                done.context("worker task panicked")??;
            }
            anyhow::Ok(())
        } => result,
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("stopping: jobs in flight are reaped back to the queue after the lock timeout");
            Ok(())
        }
    }
}

async fn run_worker(
    pool: &PgPool,
    cfg: &ClassifyConfig,
    sources: &SourcesFile,
    kinds: &[JobKind],
    name: &str,
    lock_timeout: Duration,
    drain: bool,
) -> anyhow::Result<()> {
    let mut conn = pool.acquire().await?;
    let mut last_reap: Option<tokio::time::Instant> = None;
    loop {
        if last_reap.is_none_or(|t| t.elapsed() >= REAP_EVERY) {
            let reaped = job::reap(&mut conn, lock_timeout).await?;
            if reaped.requeued + reaped.dead > 0 {
                tracing::warn!(
                    requeued = reaped.requeued,
                    dead = reaped.dead,
                    "reaped stale jobs"
                );
            }
            last_reap = Some(tokio::time::Instant::now());
        }
        let Some(claimed) = job::claim(&mut conn, kinds, name).await? else {
            if drain {
                return Ok(());
            }
            tokio::time::sleep(IDLE).await;
            continue;
        };
        match pipeline::run_classify_job(&mut conn, &claimed, cfg, sources, name).await {
            Ok(outcome) => {
                tracing::info!(job = %claimed.id, kind = %claimed.kind, outcome = ?outcome, "job done");
            }
            Err(e) => {
                let error = format!("{e:#}");
                match job::fail(&mut conn, &claimed, &error).await? {
                    Failure::Retry { run_at } => {
                        tracing::warn!(job = %claimed.id, %error, %run_at, "job failed; will retry");
                    }
                    Failure::Dead => tracing::error!(job = %claimed.id, %error, "job dead"),
                }
            }
        }
    }
}
