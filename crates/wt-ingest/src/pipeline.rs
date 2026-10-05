//! Discovery runs and the `classify` job (Spec 03, Deduplication and classification).

use chrono::Datelike;
use sqlx::types::chrono::{DateTime, NaiveDate, Utc};
use sqlx::{Acquire, PgConnection};
use wt_core::ids::{SanghaId, TalkId};
use wt_db::ingest::{self, Candidate, IngestDbError, NewVideo, PRE_GATE, Recorded};
use wt_db::job::{self, Job, JobError, JobKind};
use wt_db::registry::{self, RegistryDbError, SpeakerOutcome};

use crate::classify::{self, DupFacts, Exclusion};
use crate::config::{ClassifyConfig, ConfigError, SourceConfig, SourcesFile};
use crate::dates;
use crate::source::{DiscoveredVideo, IngestError, SourceAdapter};

/// Errors from a discovery run or a classify job.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    /// The source failed.
    #[error(transparent)]
    Source(#[from] IngestError),
    /// Configuration is missing or wrong.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// Storing or classifying failed.
    #[error(transparent)]
    Store(#[from] IngestDbError),
    /// The registry could not be read.
    #[error(transparent)]
    Registry(#[from] RegistryDbError),
    /// The job queue refused.
    #[error(transparent)]
    Job(#[from] JobError),
    /// A classify job has no talk or no source in its payload.
    #[error("classify job {0} has no talk or no source")]
    BadJob(String),
    /// The database refused.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Counts from one discovery run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveryReport {
    /// Videos listed by the source.
    pub listed: usize,
    /// New talks queued for classification.
    pub new: usize,
    /// Known talks whose metadata changed, re-queued.
    pub changed: usize,
    /// Known talks with nothing to do.
    pub unchanged: usize,
    /// Videos whose talk was withdrawn, not re-ingested.
    pub withdrawn: usize,
}

/// The input hash for a video's classify job: everything rules 1 to 6 read from the source.
/// Registry and config changes are applied by re-running classification, not by this hash.
pub fn input_hash(v: &DiscoveredVideo) -> String {
    let canonical = serde_json::json!([
        v.title,
        v.description,
        v.published_at.map(|t| t.to_rfc3339()),
        v.duration_s,
        v.has_captions
    ]);
    fnv64(canonical.to_string().as_bytes())
}

/// FNV-1a as hex: stable across runs and builds, which is all a job key needs. It is not a
/// security boundary.
fn fnv64(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The version of the classification rules in code. Raise it whenever a rule's code changes,
/// so `reclassify` applies the change to talks already classified.
pub const CLASSIFIER_VERSION: &str = "2026-10-05.recording-dates";

/// The version of everything classification reads besides the video: the rule code, the
/// pipeline and series files as written, and every alias with its teacher. A change to any of
/// them gives a new version, so `reclassify` queues work exactly when the rules changed.
pub fn rules_version(
    pipeline_toml: &str,
    series_toml: &str,
    aliases: &[(String, String)],
) -> String {
    let canonical = serde_json::json!([CLASSIFIER_VERSION, pipeline_toml, series_toml, aliases]);
    fnv64(canonical.to_string().as_bytes())
}

/// What `reclassify` found and queued.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ReclassifyReport {
    /// The rules version the jobs carry.
    pub rules_version: String,
    /// Talks eligible: before the gate, or excluded by classification.
    pub eligible: usize,
    /// Jobs queued now.
    pub queued: usize,
    /// Talks already queued or run under this rules version.
    pub already_done: usize,
    /// Talks whose source is unknown, so no sangha can be credited; left alone.
    pub no_source: usize,
}

/// Queues classification again for every talk before the gate and every talk classification
/// excluded, or only `only`, keyed by `rules_version` so unchanged rules queue nothing. An
/// excluded talk is reopened when its job runs, not here. With `dry_run` nothing is queued and
/// `queued` counts what would be.
pub async fn reclassify(
    conn: &mut PgConnection,
    rules_version: &str,
    only: Option<TalkId>,
    dry_run: bool,
) -> Result<ReclassifyReport, PipelineError> {
    let mut report = ReclassifyReport {
        rules_version: rules_version.to_owned(),
        ..ReclassifyReport::default()
    };
    let mut tx = conn.begin().await?;
    let targets = ingest::reclassify_targets(&mut tx, only).await?;
    report.eligible = targets.len();
    let key = format!("rules-{rules_version}");
    for t in targets {
        let Some(source) = t.source else {
            report.no_source += 1;
            continue;
        };
        if dry_run {
            report.queued += 1;
            continue;
        }
        let payload = serde_json::json!({ "source": source, "reopen": t.state == "excluded" });
        match job::enqueue(&mut tx, JobKind::Classify, Some(t.talk), &key, &payload).await? {
            Some(_) => report.queued += 1,
            None => report.already_done += 1,
        }
    }
    if dry_run {
        tx.rollback().await?;
    } else {
        tx.commit().await?;
    }
    Ok(report)
}

/// Lists a source and records every video, each with its talk and classify job. The whole
/// run is one transaction, so a failure part way records nothing.
pub async fn discover(
    conn: &mut PgConnection,
    source: &SourceConfig,
    adapter: &dyn SourceAdapter,
    since: Option<DateTime<Utc>>,
) -> Result<DiscoveryReport, PipelineError> {
    if source.role.as_deref() == Some("audio_only") {
        // An audio-only source attaches audio to talks another source lists; it never creates
        // one (Spec 03, Seattle Soto Zen sources).
        return Ok(DiscoveryReport::default());
    }
    let videos = adapter.discover(since).await?;
    let mut report = DiscoveryReport {
        listed: videos.len(),
        ..DiscoveryReport::default()
    };
    let mut tx = conn.begin().await?;
    for v in &videos {
        let hash = input_hash(v);
        let recorded = ingest::record_video(
            &mut tx,
            &NewVideo {
                source: &source.name,
                platform: adapter.platform().as_str(),
                external_id: &v.external_id,
                url: &v.url,
                title: &v.title,
                description: v.description.as_deref(),
                published_at: v.published_at,
                duration_s: v.duration_s,
                channel_ref: v.channel_ref.as_deref(),
                has_captions: v.has_captions,
                item: &v.raw,
                input_hash: &hash,
            },
        )
        .await?;
        match recorded {
            Recorded::New { .. } => report.new += 1,
            Recorded::Changed { .. } => report.changed += 1,
            Recorded::Unchanged { .. } => report.unchanged += 1,
            Recorded::Withdrawn { .. } => report.withdrawn += 1,
        }
    }
    tx.commit().await?;
    Ok(report)
}

/// Where classification left a talk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classified {
    /// Rule 2 excluded it.
    Excluded(Exclusion),
    /// Rule 3 merged it into the kept talk.
    Duplicate {
        /// The talk kept.
        of: TalkId,
    },
    /// Held for a person, with the reason.
    NeedsReview(String),
    /// Speaker resolved; waiting for consent.
    AwaitingConsent,
    /// Speaker resolved and the gate passed.
    Cleared,
    /// The talk had moved past classification; nothing changed.
    Skipped,
}

/// Runs rules 1 to 6 on one talk. `venue` is the sangha where the source's talks were given.
pub async fn classify_talk(
    conn: &mut PgConnection,
    cfg: &ClassifyConfig,
    talk: TalkId,
    venue: SanghaId,
    actor: &str,
) -> Result<Classified, PipelineError> {
    let facts = ingest::facts(conn, talk).await?;
    if !PRE_GATE.contains(&facts.state.as_str()) {
        return Ok(Classified::Skipped);
    }
    let title = classify::normalize_title(&facts.title);
    let date = dates::talk_date(&facts.title, facts.published_at.map(|t| t.date_naive()));
    ingest::set_talk_date(conn, talk, date).await?;

    if let Some(ex) = classify::exclusion(cfg, &title, facts.duration_s) {
        ingest::exclude(conn, talk, ex.reason(), ex.talk_type(), None, actor).await?;
        return Ok(Classified::Excluded(ex));
    }

    let series = classify::series(cfg, &title);
    let part = series.as_ref().and_then(|(_, p)| *p);
    if let Some(kept) = merge_duplicates(
        conn,
        cfg,
        talk,
        &title,
        date,
        &facts,
        series.as_ref(),
        actor,
    )
    .await?
    {
        return Ok(Classified::Duplicate { of: kept });
    }

    let decision = classify::talk_type(cfg, &title, facts.duration_s);
    ingest::set_classification(
        conn,
        talk,
        &decision.talk_type,
        series.as_ref().map(|(s, _)| s.as_str()),
        part,
        decision.review.as_deref(),
        actor,
    )
    .await?;
    if let Some(reason) = decision.review {
        return Ok(Classified::NeedsReview(reason));
    }

    let matcher = registry::alias_matcher(conn).await?;
    Ok(
        match registry::assign_speaker(conn, &matcher, talk, venue, actor).await? {
            SpeakerOutcome::AwaitingConsent => Classified::AwaitingConsent,
            SpeakerOutcome::Cleared => Classified::Cleared,
            SpeakerOutcome::NeedsReview => {
                let reason = ingest::facts(conn, talk)
                    .await?
                    .state_reason
                    .unwrap_or_default();
                Classified::NeedsReview(reason)
            }
        },
    )
}

/// Rule 3. Finds this talk's duplicates and keeps one record of the cluster: a talk already
/// past the gate if there is one (it cannot be demoted), otherwise one with captions, otherwise
/// the earliest published. The others before the gate become duplicates of it. Returns the
/// kept talk when it is not this one.
#[allow(clippy::too_many_arguments)]
async fn merge_duplicates(
    conn: &mut PgConnection,
    cfg: &ClassifyConfig,
    talk: TalkId,
    title: &str,
    date: Option<NaiveDate>,
    facts: &ingest::ClassifyFacts,
    series: Option<&(String, Option<i32>)>,
    actor: &str,
) -> Result<Option<TalkId>, PipelineError> {
    let Some(duration) = facts.duration_s else {
        return Ok(None);
    };
    let s = &cfg.settings;
    let pct_gap = (f64::from(duration) * f64::from(s.dedupe_duration_tolerance_pct) / 50.0).ceil();
    let max_gap = s
        .dedupe_title_identical_tolerance_s
        .max(s.dedupe_duration_tolerance_min_s)
        .max(i32::try_from(pct_gap as i64).unwrap_or(i32::MAX));
    let me = DupFacts {
        title: title.to_owned(),
        day: date.map(day),
        duration_s: Some(duration),
        series_part: series.and_then(|(n, p)| p.map(|p| (n.clone(), p))),
    };
    let cluster: Vec<Candidate> =
        ingest::duplicate_candidates(conn, talk, title, duration, max_gap)
            .await?
            .into_iter()
            .filter(|c| {
                let other = DupFacts {
                    title: c.normalized_title.clone(),
                    // Like the series below, from the title: the other talk's stored date
                    // may not be set yet.
                    day: dates::talk_date(&c.title, c.published_at.map(|t| t.date_naive()))
                        .map(day),
                    duration_s: c.duration_s,
                    // From the title, not the stored columns: the other talk may not be
                    // classified yet, and the result must not depend on job order.
                    series_part: classify::series(cfg, &c.normalized_title)
                        .and_then(|(name, part)| part.map(|p| (name, p))),
                };
                classify::is_duplicate(cfg, &me, &other, c.similarity)
            })
            .collect();
    if cluster.is_empty() {
        return Ok(None);
    }

    // Lower sorts first: past the gate, then captions, then earliest, then id for stability.
    let rank = |state: &str, captions: bool, at: Option<DateTime<Utc>>, id: TalkId| {
        (
            PRE_GATE.contains(&state),
            !captions,
            at.is_none(),
            at,
            id.as_uuid(),
        )
    };
    let mine = rank(&facts.state, facts.has_captions, facts.published_at, talk);
    let best = cluster
        .iter()
        .map(|c| {
            (
                rank(&c.state, c.has_captions, c.published_at, c.talk),
                c.talk,
            )
        })
        .chain(std::iter::once((mine, talk)))
        .min_by(|a, b| a.0.cmp(&b.0))
        .map_or(talk, |(_, id)| id);

    let mut losers: Vec<TalkId> = cluster
        .iter()
        .filter(|c| c.talk != best && PRE_GATE.contains(&c.state.as_str()))
        .map(|c| c.talk)
        .collect();
    if best != talk {
        losers.push(talk);
    }
    for loser in losers {
        ingest::exclude(conn, loser, "duplicate", None, Some(best), actor).await?;
    }
    Ok((best != talk).then_some(best))
}

fn day(date: NaiveDate) -> i64 {
    i64::from(date.num_days_from_ce())
}

/// Runs one claimed `classify` job in a transaction with its completion, so the talk's new
/// state and the job's success commit together.
pub async fn run_classify_job(
    conn: &mut PgConnection,
    job: &Job,
    cfg: &ClassifyConfig,
    sources: &SourcesFile,
    actor: &str,
) -> Result<Classified, PipelineError> {
    let talk = job
        .talk_id
        .ok_or_else(|| PipelineError::BadJob(job.id.to_string()))?;
    let source = job
        .payload
        .get("source")
        .and_then(|s| s.as_str())
        .ok_or_else(|| PipelineError::BadJob(job.id.to_string()))?;
    debug_assert_eq!(job.kind, JobKind::Classify);
    let reopen = job
        .payload
        .get("reopen")
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    let mut tx = conn.begin().await?;
    let venue = registry::find_sangha(&mut tx, &sources.get(source)?.default_sangha).await?;
    if reopen {
        ingest::reopen(&mut tx, talk, actor).await?;
    }
    let outcome = classify_talk(&mut tx, cfg, talk, venue, actor).await?;
    job::succeed(&mut tx, job).await?;
    tx.commit().await?;
    Ok(outcome)
}
