//! Cutting each chosen passage's excerpt for the question (Spec 05, Answer assembly, rule 8):
//! the model step, then the reranker step, then the trimmed passage. Every passage of an answer
//! runs at once, each step under `excerpt_timeout_s`. Model calls count against the spend cap
//! through the client, and a real talk's text goes to the model only when
//! `WT_LLM_REAL_DATA_OK` is true.

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::future::join_all;
use wt_core::excerpt::{Chooser, Excerpt, Limits, MAX_EXCERPT_WORDS, Window};
use wt_db::answer::Served;
use wt_extract::DataClass;
use wt_retrieve::ExcerptConfig;

use crate::Site;

/// The limits `[excerpt]` sets.
pub fn limits(cfg: &ExcerptConfig) -> Limits {
    Limits {
        min_words: cfg.excerpt_min_words,
        max_words: MAX_EXCERPT_WORDS,
    }
}

/// One window's excerpt, with its span.
async fn choose_one(site: &Site, query: &str, window: &Window) -> Excerpt {
    let cfg = &site.retrieval.excerpt;
    let limits = limits(cfg);
    let timeout = Duration::from_secs(cfg.excerpt_timeout_s);
    if site.real_data_ok {
        match wt_synth::excerpt::choose(
            site.llm.as_ref(),
            query,
            window,
            limits,
            timeout,
            DataClass::Real,
        )
        .await
        {
            Ok((s, e)) => {
                return Excerpt::build(window, s, e, Some(window.span(s, e, Chooser::Model)));
            }
            Err(miss) => tracing::info!(reason = miss.as_str(), "excerpt model step gave no run"),
        }
    }
    if let Some(reranker) = site.reranker.as_deref() {
        let step = wt_retrieve::excerpt::choose(
            reranker,
            query,
            window,
            limits,
            cfg.excerpt_max_spans,
            cfg.excerpt_keep_ratio,
        );
        match tokio::time::timeout(timeout, step).await {
            Ok(Ok((s, e))) => {
                return Excerpt::build(window, s, e, Some(window.span(s, e, Chooser::Reranker)));
            }
            Ok(Err(e)) => tracing::info!(error = %e, "excerpt reranker step gave no span"),
            Err(_) => tracing::info!("excerpt reranker step timed out"),
        }
    }
    let (s, e) = window.trimmed(limits);
    Excerpt::build(window, s, e, Some(window.span(s, e, Chooser::Passage)))
}

/// Each served passage's excerpt, keyed by passage. A passage without a window (it stopped
/// serving between loading and now) shows whole.
pub async fn choose(
    site: &Site,
    query: &str,
    served: &[Served],
    windows: &[Window],
) -> BTreeMap<wt_core::ids::PassageId, Excerpt> {
    let chosen = join_all(windows.iter().map(|w| choose_one(site, query, w))).await;
    let mut out: BTreeMap<_, _> = windows
        .iter()
        .zip(chosen)
        .filter_map(|(w, e)| w.passages.get(w.selected).map(|p| (p.id, e)))
        .collect();
    for s in served {
        out.entry(s.id).or_insert_with(|| {
            Excerpt::whole(&wt_core::excerpt::WindowPassage {
                id: s.id,
                text: s.text.clone(),
                ordinal: s.ordinal,
                start_ms: s.start_ms,
                sentence_starts: Vec::new(),
                sentence_start_ms: Vec::new(),
            })
        });
    }
    let mut by: BTreeMap<&str, usize> = BTreeMap::new();
    for e in out.values() {
        let key = e.span.as_ref().map_or("whole", |s| s.chooser.as_str());
        *by.entry(key).or_default() += 1;
    }
    tracing::info!(choosers = ?by, "excerpts chosen");
    out
}

/// The excerpts of a stored answer, rebuilt from serving rows by their spans. A passage whose
/// span no longer fits, or that has none, shows whole.
pub fn rebuild(
    spans: &[wt_core::excerpt::ExcerptSpan],
    served: &[Served],
    rows: &[wt_core::excerpt::WindowPassage],
) -> BTreeMap<wt_core::ids::PassageId, Excerpt> {
    served
        .iter()
        .map(|s| {
            let excerpt = spans
                .iter()
                .find(|sp| sp.passage_id == s.id)
                .and_then(|sp| Excerpt::rebuild(sp, rows))
                .or_else(|| rows.iter().find(|r| r.id == s.id).map(Excerpt::whole))
                .unwrap_or_else(|| {
                    Excerpt::whole(&wt_core::excerpt::WindowPassage {
                        id: s.id,
                        text: s.text.clone(),
                        ordinal: s.ordinal,
                        start_ms: s.start_ms,
                        sentence_starts: Vec::new(),
                        sentence_start_ms: Vec::new(),
                    })
                });
            (s.id, excerpt)
        })
        .collect()
}
