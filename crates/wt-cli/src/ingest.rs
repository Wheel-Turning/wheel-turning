//! `wt ingest discover` and `wt worker` (Spec 03).

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, bail};
use sqlx::PgPool;
use wt_core::{Config, Secret};
use wt_db::job::{self, Failure, JobKind};
use wt_ingest::config::{ClassifyConfig, SourceConfig, SourcesFile};
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

/// A Vimeo source's own access token, from the variable its `token_env` names (owner's
/// decision of October 13, 2026: each sangha's Vimeo account has its own variable).
pub fn vimeo_token(config: &Config, source: &SourceConfig) -> Option<Secret> {
    source
        .token_env
        .as_deref()
        .and_then(|key| config.source_secret(key))
}

/// Route (b) of Spec 03 for the media fetcher: the Vimeo API and the token of every Vimeo
/// source in `sources.toml` whose variable is set.
pub fn vimeo_media(
    config: &Config,
    config_dir: &Path,
) -> anyhow::Result<wt_ingest::media::VimeoMedia> {
    let sources = SourcesFile::load(&config_dir.join("sources.toml"))?;
    let tokens = sources
        .sources
        .iter()
        .filter(|s| s.platform == SourcePlatform::Vimeo)
        .filter_map(|s| Some((s.name.clone(), vimeo_token(config, s)?)))
        .collect();
    Ok(wt_ingest::media::VimeoMedia {
        api: VimeoEndpoints::vimeo()?.api,
        tokens,
    })
}

/// Route (c) of Spec 03 for the media fetcher: the root of every storage source in
/// `sources.toml`, relative to the directory `wt` runs in.
pub fn storage_media(config_dir: &Path) -> anyhow::Result<wt_ingest::media::StorageMedia> {
    let sources = SourcesFile::load(&config_dir.join("sources.toml"))?;
    let roots = sources
        .sources
        .iter()
        .filter(|s| s.platform == SourcePlatform::Storage)
        .filter_map(|s| Some((s.name.clone(), std::path::PathBuf::from(s.root.as_deref()?))))
        .collect();
    Ok(wt_ingest::media::StorageMedia { roots })
}

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
                vimeo_token(config, source),
                source.talks_page.as_deref(),
                VimeoEndpoints::vimeo()?,
                exclusions.clone(),
            )?),
            SourcePlatform::EngageWisdom => {
                let Some(collection) = source.collection_url.as_deref() else {
                    bail!("source {} has no collection_url", source.name);
                };
                // A whole collection takes hours at the crawl delay: talks are recorded in
                // batches, and a rerun skips those already recorded.
                let known = {
                    let mut conn = pool.acquire().await?;
                    wt_db::ingest::recorded_urls(&mut conn, &source.name).await?
                };
                let adapter = wt_ingest::engagewisdom::EngageWisdomAdapter::new(
                    collection,
                    source.max_talks,
                    exclusions.clone(),
                )?
                .with_known(known)
                .with_batch(EW_BATCH);
                let mut total = pipeline::DiscoveryReport::default();
                loop {
                    let mut conn = pool.acquire().await?;
                    let report = pipeline::discover(&mut conn, source, &adapter, None)
                        .await
                        .with_context(|| format!("discovering {}", source.name))?;
                    if report.listed == 0 {
                        break;
                    }
                    total.listed += report.listed;
                    total.new += report.new;
                    total.changed += report.changed;
                    total.unchanged += report.unchanged;
                    total.withdrawn += report.withdrawn;
                    tracing::info!(source = %source.name, recorded = total.listed, "discovery batch recorded");
                }
                reports.insert(source.name.clone(), report_json(&total));
                continue;
            }
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
            // Storage files never create talks: match-storage pairs them with listed talks.
            SourcePlatform::Storage => {
                if only.is_some() {
                    bail!(
                        "source {} is storage: run `wt ingest match-storage --source {}`",
                        source.name,
                        source.name
                    );
                }
                continue;
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
        reports.insert(source.name.clone(), report_json(&report));
    }
    print_json(&reports)
}

/// Talks an EngageWisdom discovery records at a time.
const EW_BATCH: usize = 25;

/// A discovery report's counts as JSON for one source.
fn report_json(report: &pipeline::DiscoveryReport) -> serde_json::Value {
    serde_json::json!({
        "listed": report.listed, "new": report.new, "changed": report.changed,
        "unchanged": report.unchanged, "withdrawn": report.withdrawn,
    })
}

/// Runs jobs of the named kinds with `concurrency` workers until stopped, or with `drain`
/// until nothing is due. Each worker reaps stale locks once a minute.
pub async fn worker(
    pool: &PgPool,
    config: &Config,
    config_dir: &Path,
    kinds: &[JobKind],
    concurrency: usize,
    drain: bool,
    only_sources: &[String],
) -> anyhow::Result<()> {
    let cfg = ClassifyConfig::load(
        &config_dir.join("pipeline.toml"),
        &config_dir.join("series.toml"),
    )?;
    let stage_kinds: Vec<JobKind> = kinds
        .iter()
        .copied()
        .filter(|k| *k != JobKind::Classify)
        .collect();
    let stages = if stage_kinds.is_empty() {
        None
    } else {
        Some(std::sync::Arc::new(
            crate::stages::Stages::build(pool, config, config_dir, &cfg, &stage_kinds, "wt worker")
                .await?,
        ))
    };
    let sources = SourcesFile::load(&config_dir.join("sources.toml"))?;
    let lock_timeout =
        Duration::from_secs(u64::try_from(cfg.settings.job_lock_timeout_s).unwrap_or(7200));
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "worker".to_owned());

    let mut tasks = tokio::task::JoinSet::new();
    for n in 0..concurrency.max(1) {
        let (pool, cfg, sources, kinds, stages, only_sources) = (
            pool.clone(),
            cfg.clone(),
            sources.clone(),
            kinds.to_vec(),
            stages.clone(),
            only_sources.to_vec(),
        );
        let name = format!("{host}-{}-{n}", std::process::id());
        tasks.spawn(async move {
            run_worker(
                &pool,
                &cfg,
                &sources,
                stages.as_deref(),
                &kinds,
                &only_sources,
                &name,
                lock_timeout,
                drain,
            )
            .await
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

/// The worker loop: claims jobs of `kinds` one at a time and runs each, reaping stale locks older
/// than `lock_timeout` as it goes. With `drain` it stops when no job is left; otherwise it waits
/// for more.
#[allow(clippy::too_many_arguments)]
async fn run_worker(
    pool: &PgPool,
    cfg: &ClassifyConfig,
    sources: &SourcesFile,
    stages: Option<&crate::stages::Stages>,
    kinds: &[JobKind],
    only_sources: &[String],
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
        let Some(claimed) = job::claim_from(&mut conn, kinds, only_sources, name).await? else {
            if let Some(stages) = stages {
                stages.idle(&mut conn).await?;
            }
            if drain {
                return Ok(());
            }
            tokio::time::sleep(IDLE).await;
            continue;
        };
        let result: anyhow::Result<()> = if claimed.kind == JobKind::Classify {
            job::with_heartbeat(
                pool,
                &claimed,
                job::HEARTBEAT_EVERY,
                pipeline::run_classify_job(&mut conn, &claimed, cfg, sources, name),
            )
            .await
                .map(|outcome| {
                    tracing::info!(job = %claimed.id, kind = %claimed.kind, outcome = ?outcome, "job done");
                })
                .map_err(Into::into)
        } else {
            match stages {
                None => Err(anyhow::anyhow!("no handler for {} jobs", claimed.kind)),
                Some(stages) => match job::with_heartbeat(
                    pool,
                    &claimed,
                    job::HEARTBEAT_EVERY,
                    stages.run(&mut conn, &claimed),
                )
                .await
                {
                    Ok(crate::stages::Ran::Done(outcome)) => {
                        job::succeed(&mut conn, &claimed).await?;
                        tracing::info!(job = %claimed.id, kind = %claimed.kind, %outcome, "job done");
                        Ok(())
                    }
                    Ok(crate::stages::Ran::Capped(reason)) => {
                        let until = wt_db::llm::next_month(chrono::Utc::now());
                        job::defer(&mut conn, &claimed, until, "cap_reached").await?;
                        tracing::warn!(job = %claimed.id, kind = %claimed.kind, %reason, %until,
                                       "language-model cap reached; job waits for next month");
                        Ok(())
                    }
                    Err(e) => Err(e),
                },
            }
        };
        match result {
            Ok(()) => {}
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
        vimeo_token(config, source),
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
            format!(
                "nowhere: set {} or pass --vimeo-list",
                source
                    .token_env
                    .as_deref()
                    .unwrap_or("the source's token_env")
            ),
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

/// `wt ingest match-storage` (Spec 03, the storage adapter): lists the storage source's files,
/// proposes a Vimeo talk for each, and writes the review table to `<out>/<source>.csv` and
/// `.html`. With `apply`, records every proposed match; with `sheet`, records the rows the
/// reviewer marked `yes`. The sheet is read before the table is written again, so the reviewed
/// file can be the one in `out`. Prints counts as JSON.
pub async fn match_storage(
    pool: &PgPool,
    config_dir: &Path,
    source: &str,
    out: &Path,
    apply: bool,
    sheet: Option<&Path>,
) -> anyhow::Result<()> {
    use wt_ingest::storage::{self, Ffprobe};
    use wt_ingest::storagematch;

    let sources = SourcesFile::load(&config_dir.join("sources.toml"))?;
    let source = sources.get(source)?;
    if source.platform != SourcePlatform::Storage {
        bail!(
            "source {} is {}; match-storage reads a storage source",
            source.name,
            source.platform
        );
    }
    let root = std::path::PathBuf::from(source.root.as_deref().unwrap_or_default());
    if !root.is_dir() {
        bail!(
            "no folder at {} (the root of {} in sources.toml): put the files there first, as \
             docs/runbook.md says under \"When SSZ's files arrive\"",
            root.display(),
            source.name
        );
    }
    let vimeo: Vec<String> = sources
        .sources
        .iter()
        .filter(|s| {
            s.platform == SourcePlatform::Vimeo && s.default_sangha == source.default_sangha
        })
        .map(|s| s.name.clone())
        .collect();
    if vimeo.is_empty() {
        bail!(
            "no Vimeo source of {} to match {} to",
            source.default_sangha,
            source.name
        );
    }
    let cfg = ClassifyConfig::load(
        &config_dir.join("pipeline.toml"),
        &config_dir.join("series.toml"),
    )?;
    let reviewed = match sheet {
        Some(path) => Some(storagematch::read_sheet(
            &std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?,
        )?),
        None => None,
    };
    let files = storage::list(&root, &source.storage_extensions(), &Ffprobe::default())?;
    let mut conn = pool.acquire().await?;
    let mut review = storagematch::review(&mut conn, &cfg, &source.name, &vimeo, &files).await?;
    let applied = if apply || reviewed.is_some() {
        let rows = storagematch::confirmed(&review, reviewed.as_deref());
        let applied = storagematch::apply(
            &mut conn,
            &source.name,
            &vimeo,
            &files,
            &rows,
            "wt ingest match-storage",
        )
        .await?;
        review = storagematch::review(&mut conn, &cfg, &source.name, &vimeo, &files).await?;
        Some(applied)
    } else {
        None
    };
    let generated = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();
    let csv = storagematch::write_review(out, &review, &generated)
        .with_context(|| format!("writing the review table to {}", out.display()))?;
    print_json(&serde_json::json!({
        "source": source.name,
        "root": root.display().to_string(),
        "files": review.files,
        "rows": review.counts,
        "review_csv": csv.display().to_string(),
        "review_html": csv.with_extension("html").display().to_string(),
        "applied": applied,
        "next": if applied.is_some() {
            "wt pipeline retry-held --reason no_media, then wt pipeline advance"
        } else {
            "review the table, then run again with --apply or --sheet <reviewed csv>"
        },
    }))
}
