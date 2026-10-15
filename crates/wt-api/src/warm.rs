//! Keeps the vector and full-text indexes in the database's cache (owner's request of October
//! 15, 2026: answers under 5 seconds). A question whose search reads index pages that are not
//! cached reads them from disk, which took 2 to 7 s on the beta. At startup the server runs a
//! vector search and a full-text search for each of a set of practice phrases, which touches
//! most of the vector index and the full-text pages questions read, and then one pair every few
//! minutes, so a quiet hour does not let them go cold. The searches read passage ids only,
//! through `servable_passage`, and nothing is stored or logged but their timing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::PgPool;
use wt_ml::Embedder;
use wt_retrieve::config::SearchConfig;
use wt_retrieve::search;

/// Phrases close to what testers ask, spread over the subjects the talks cover.
const PHRASES: [&str; 24] = [
    "how do I work with anger",
    "my mind wanders during zazen",
    "grief after my mother died",
    "fear of death",
    "pain in my knees while sitting",
    "I feel sad and lonely",
    "anxiety at work",
    "what is shikantaza",
    "how to practice with a difficult relationship",
    "doubt about practice",
    "sleepiness in meditation",
    "what is enlightenment",
    "the precepts and ethics",
    "koan practice with mu",
    "compassion for myself",
    "jealousy and comparison",
    "forgiveness",
    "impermanence and change",
    "chanting the heart sutra",
    "raising children as practice",
    "illness and the body",
    "desire and craving",
    "being present in daily life",
    "the teacher and student relationship",
];

/// Searches between keep-warm rounds.
const EVERY: Duration = Duration::from_secs(300);

/// Warms the indexes once, then keeps them warm. Failures are logged and the loop goes on.
pub async fn keep_warm(pool: PgPool, embedder: Arc<dyn Embedder>, cfg: SearchConfig) {
    let mut vectors = Vec::with_capacity(PHRASES.len());
    for phrase in PHRASES {
        match embedder.embed(&[phrase.to_owned()]).await {
            Ok(mut v) if !v.is_empty() => vectors.push((phrase, v.swap_remove(0))),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "embedding a warm-up phrase"),
        }
    }
    if vectors.is_empty() {
        return;
    }
    let started = Instant::now();
    for (phrase, v) in &vectors {
        search_once(&pool, phrase, v, &cfg).await;
    }
    tracing::info!(
        searches = vectors.len(),
        ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "search indexes warmed"
    );
    let mut next = 0;
    loop {
        tokio::time::sleep(EVERY).await;
        if let Some((phrase, v)) = vectors.get(next % vectors.len()) {
            search_once(&pool, phrase, v, &cfg).await;
        }
        next += 1;
    }
}

/// One standard-mode vector search and one full-text search, their results dropped.
async fn search_once(pool: &PgPool, phrase: &str, vector: &[f32], cfg: &SearchConfig) {
    let found = async {
        let mut conn = pool.acquire().await?;
        search::vector(
            &mut conn,
            vector,
            None,
            cfg.vector_top_k,
            cfg.hnsw_ef_search,
        )
        .await?;
        if let Some(q) = search::or_query(phrase) {
            search::full_text(&mut conn, &q, None, cfg.fts_top_k, cfg.fts_max_lexeme_share).await?;
        }
        Ok::<_, sqlx::Error>(())
    }
    .await;
    if let Err(e) = found {
        tracing::warn!(error = %e, "warm-up search");
    }
}
