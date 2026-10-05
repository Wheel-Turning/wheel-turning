//! `wt ingest discover` and `wt worker` (Spec 03).

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, bail};
use sqlx::PgPool;
use wt_core::Config;
use wt_db::job::{self, Failure, JobKind};
use wt_ingest::config::{ClassifyConfig, SourcesFile};
use wt_ingest::http::Exclusions;
use wt_ingest::pipeline;
use wt_ingest::rss::RssAdapter;
use wt_ingest::vimeo::{VimeoAdapter, VimeoEndpoints};
use wt_ingest::{SourceAdapter, SourcePlatform};

use crate::commands::print_json;

/// How long an idle worker waits before looking for work again.
const IDLE: Duration = Duration::from_secs(5);
/// How often a worker runs the stale-lock reaper.
const REAP_EVERY: Duration = Duration::from_secs(60);

/// The active exclusions, which every request is checked against (Spec 08).
pub async fn load_exclusions(pool: &PgPool) -> anyhow::Result<Exclusions> {
    let mut conn = pool.acquire().await?;
    Exclusions::load(&mut conn)
        .await
        .context("loading the active exclusions")
}

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
    let exclusions = load_exclusions(pool).await?;
    // Audio-only feeds attach to talks other sources list, so they run after those sources.
    let mut chosen: Vec<_> = sources
        .sources
        .iter()
        .filter(|s| only.is_none_or(|n| n == s.name))
        .collect();
    chosen.sort_by_key(|s| s.is_audio_only());
    for source in chosen {
        let adapter: Box<dyn SourceAdapter> = match source.platform {
            SourcePlatform::Vimeo => Box::new(VimeoAdapter::new(
                &source.name,
                config.vimeo_access_token.clone(),
                source.talks_page.as_deref(),
                VimeoEndpoints::vimeo()?,
                exclusions.clone(),
            )?),
            SourcePlatform::Rss => {
                let Some(feed_url) = source.feed_url.as_deref() else {
                    if only.is_some() {
                        bail!(
                            "source {} has no feed_url yet: run `wt source resolve-feed` first",
                            source.name
                        );
                    }
                    tracing::info!(source = %source.name, "no feed_url yet; skipped");
                    continue;
                };
                let adapter = RssAdapter::new(feed_url, exclusions.clone())?;
                if source.is_audio_only() {
                    let cfg = ClassifyConfig::load(
                        &config_dir.join("pipeline.toml"),
                        &config_dir.join("series.toml"),
                    )?;
                    let vimeo: Vec<String> = sources
                        .sources
                        .iter()
                        .filter(|s| {
                            s.platform == SourcePlatform::Vimeo
                                && s.default_sangha == source.default_sangha
                        })
                        .map(|s| s.name.clone())
                        .collect();
                    let mut conn = pool.acquire().await?;
                    let report = pipeline::discover_feed(
                        &mut conn,
                        &cfg,
                        source,
                        &adapter,
                        &vimeo,
                        "wt ingest discover",
                    )
                    .await
                    .with_context(|| format!("reading feed {}", source.name))?;
                    reports.insert(source.name.clone(), serde_json::to_value(&report)?);
                    continue;
                }
                Box::new(adapter)
            }
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

/// Writes the review page to `out`. Reads only: it changes nothing in the database.
pub async fn report(pool: &PgPool, config_dir: &Path, out: &Path) -> anyhow::Result<()> {
    let cfg = ClassifyConfig::load(
        &config_dir.join("pipeline.toml"),
        &config_dir.join("series.toml"),
    )?;
    let mut conn = pool.acquire().await?;
    let rows = wt_db::ingest::report_rows(&mut conn).await?;
    let report = wt_ingest::report::build(&rows, &cfg.series, sqlx::types::chrono::Utc::now());
    std::fs::write(out, wt_ingest::report::render_html(&report))
        .with_context(|| format!("writing {}", out.display()))?;
    tracing::info!(
        out = %out.display(),
        talks = report.totals.talks,
        duplicate_clusters = report.duplicate_clusters.len(),
        needs_review = report.needs_review.len(),
        "ingest report written"
    );
    Ok(())
}

/// Queues classification again for talks before the gate and talks classification excluded,
/// when the rules (pipeline.toml, series.toml or the registry's aliases) have changed since
/// their last run. Prints the counts as JSON. A worker then runs the jobs.
pub async fn reclassify(
    pool: &PgPool,
    config_dir: &Path,
    talk: Option<wt_core::ids::TalkId>,
    dry_run: bool,
) -> anyhow::Result<()> {
    let read = |name: &str| {
        let path = config_dir.join(name);
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
    };
    let (pipeline_toml, series_toml) = (read("pipeline.toml")?, read("series.toml")?);
    // Refuse rules that do not load before queuing work that would fail on them.
    ClassifyConfig::parse(&pipeline_toml, "pipeline.toml", &series_toml, "series.toml")?;
    let mut conn = pool.acquire().await?;
    let aliases = wt_db::ingest::alias_list(&mut conn).await?;
    let version = pipeline::rules_version(&pipeline_toml, &series_toml, &aliases);
    let report = pipeline::reclassify(&mut conn, &version, talk, dry_run).await?;
    print_json(&report)
}

/// Compares the Vimeo account list with the talks page and with what discovery recorded, and
/// writes the completeness page. The account list comes from the API when a token is set,
/// otherwise from account pages a person saved (`saved_lists`). Prints the counts and the ids
/// on the talks page but not in the account list as JSON. Reads only.
pub async fn completeness(
    pool: &PgPool,
    config: &Config,
    config_dir: &Path,
    source: &str,
    saved_lists: &[std::path::PathBuf],
    out: &Path,
) -> anyhow::Result<()> {
    use wt_ingest::completeness::{self, RecordedVideo};

    let sources = SourcesFile::load(&config_dir.join("sources.toml"))?;
    let source = sources.get(source)?;
    if source.platform != SourcePlatform::Vimeo {
        bail!(
            "source {} is {}, and the completeness check compares Vimeo lists",
            source.name,
            source.platform
        );
    }
    let adapter = VimeoAdapter::new(
        &source.name,
        config.vimeo_access_token.clone(),
        source.talks_page.as_deref(),
        VimeoEndpoints::vimeo()?,
        load_exclusions(pool).await?,
    )?;

    let (account, from) = match adapter.account_videos().await? {
        Some(videos) => (
            Some(
                videos
                    .into_iter()
                    .map(|v| completeness::ListedVideo {
                        id: v.external_id,
                        title: Some(v.title),
                        duration_s: v.duration_s,
                    })
                    .collect::<Vec<_>>(),
            ),
            "the Vimeo API".to_owned(),
        ),
        None if !saved_lists.is_empty() => {
            let mut lists = Vec::new();
            for path in saved_lists {
                let html = std::fs::read_to_string(path)
                    .with_context(|| format!("reading {}", path.display()))?;
                lists.push(completeness::parse_saved_account_page(&html));
            }
            (
                Some(completeness::merge_lists(lists)),
                format!("{} saved account page(s)", saved_lists.len()),
            )
        }
        None => (
            None,
            "nowhere: set WT_VIMEO_ACCESS_TOKEN or pass --vimeo-list".to_owned(),
        ),
    };
    let page_ids = if source.talks_page.is_some() {
        adapter.talks_page_ids().await?
    } else {
        Vec::new()
    };
    let mut conn = pool.acquire().await?;
    let recorded: Vec<RecordedVideo> = wt_db::ingest::report_rows(&mut conn)
        .await?
        .into_iter()
        .filter(|r| r.platform == "vimeo")
        .map(|r| RecordedVideo {
            id: r.external_id,
            title: r.title,
            state: r.state,
        })
        .collect();
    let feed_source = sources.sources.iter().find(|s| {
        s.platform == SourcePlatform::Rss
            && s.is_audio_only()
            && s.default_sangha == source.default_sangha
    });
    let episodes: Vec<completeness::FeedEpisode> = match feed_source {
        Some(f) => wt_db::feed::episodes(&mut conn, &f.name)
            .await?
            .into_iter()
            .map(|e| completeness::FeedEpisode {
                id: e.external_id,
                title: e.title,
                duration_s: e.duration_s,
                matched_vimeo_id: e.matched_vimeo_id,
            })
            .collect(),
        None => Vec::new(),
    };
    let (feed, note) = match feed_source {
        None => (
            None,
            "No podcast feed source is set up for this sangha.".to_owned(),
        ),
        Some(f) if episodes.is_empty() => (
            None,
            format!(
                "Feed source {name} has no episodes recorded yet. Resolve its address with \
                 `wt source resolve-feed`, then run `wt ingest discover --source {name}`.",
                name = f.name
            ),
        ),
        Some(f) => (Some((f.name.as_str(), episodes.as_slice())), String::new()),
    };
    let mut report = completeness::compare(account.as_deref(), &page_ids, &recorded, feed, &note);
    report.account_list_source = from;

    let now = sqlx::types::chrono::Utc::now()
        .format("%Y-%m-%d %H:%M UTC")
        .to_string();
    std::fs::write(out, completeness::render_html(&report, &now))
        .with_context(|| format!("writing {}", out.display()))?;
    let ids =
        |entries: &[completeness::Entry]| entries.iter().map(|e| e.id.clone()).collect::<Vec<_>>();
    print_json(&serde_json::json!({
        "account_list": report.account_list_source,
        "account": report.account_count,
        "account_hours": report.account_hours,
        "talks_page": report.talks_page_count,
        "recorded": report.recorded_count,
        "known": report.known_count,
        "on_page_not_in_account": ids(&report.page_not_in_account),
        "in_account_not_on_page": report.account_not_on_page.len(),
        "listed_not_recorded": ids(&report.listed_not_recorded),
        "recorded_not_listed": ids(&report.recorded_not_listed),
        "feed": report.feed.as_ref().map(|f| serde_json::json!({
            "source": f.source,
            "episodes": f.episodes,
            "matched": f.matched,
            "vimeo_missing_from_feed": f.vimeo_missing_from_feed.len(),
            "episodes_unmatched": f.episodes_unmatched.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
        })),
    }))
}
