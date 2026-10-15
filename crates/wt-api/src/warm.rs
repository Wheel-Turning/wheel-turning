//! Keeps the vector index in the database's cache (owner's request of October 15, 2026: answers
//! under 5 seconds). A question whose search walks index pages that are not cached reads them
//! from disk, which took 2 to 7 s on the beta. At startup the server runs a vector search for
//! each of a set of practice phrases, which touches most of the index, and then one every few
//! minutes, so a quiet hour does not let it go cold. The searches read passage ids only, through
//! `servable_passage`, and nothing is stored or logged but their timing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::PgPool;
use wt_ml::Embedder;
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

/// Warms the index once, then keeps it warm. Failures are logged and the loop goes on.
pub async fn keep_warm(pool: PgPool, embedder: Arc<dyn Embedder>, ef_search: i64, k: i64) {
    let mut vectors = Vec::with_capacity(PHRASES.len());
    for phrase in PHRASES {
        match embedder.embed(&[phrase.to_owned()]).await {
            Ok(mut v) if !v.is_empty() => vectors.push(v.swap_remove(0)),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "embedding a warm-up phrase"),
        }
    }
    if vectors.is_empty() {
        return;
    }
    let started = Instant::now();
    for v in &vectors {
        search_once(&pool, v, ef_search, k).await;
    }
    tracing::info!(
        searches = vectors.len(),
        ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "vector index warmed"
    );
    let mut next = 0;
    loop {
        tokio::time::sleep(EVERY).await;
        if let Some(v) = vectors.get(next % vectors.len()) {
            search_once(&pool, v, ef_search, k).await;
        }
        next += 1;
    }
}

/// One standard-mode vector search, its result dropped.
async fn search_once(pool: &PgPool, vector: &[f32], ef_search: i64, k: i64) {
    let found = async {
        let mut conn = pool.acquire().await?;
        search::vector(&mut conn, vector, None, k, ef_search).await
    }
    .await;
    if let Err(e) = found {
        tracing::warn!(error = %e, "warm-up vector search");
    }
}
