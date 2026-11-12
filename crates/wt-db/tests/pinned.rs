//! Spec 10, D3: pinned answers. Which answers can be pinned, the round trip, and takedown.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{ACTOR, ADA, BO, consent, make_ready, talk};
use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::gate::ConsentStatus;
use wt_core::ids::{AnswerId, PassageId};
use wt_db::pinned::{PinError, find, list, pin, source, unpin};
use wt_db::takedown::{Scope, run};

const QUESTION: &str = "What is the difference between practice and trying to improve myself?";

/// A stored answer to `question` over `passages`, with a one-sentence summary unless `summary`
/// is false.
async fn stored(
    pool: &PgPool,
    question: Option<&str>,
    passages: &[PassageId],
    summary: bool,
    category: &str,
) -> AnswerId {
    let tester: Uuid = sqlx::query_scalar(
        "insert into tester (label, token_hash) values ('fixture', decode(md5(random()::text), 'hex'))
         returning id",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let log: Uuid = sqlx::query_scalar(
        "insert into query_log (tester_id, query_text, mode, teacher_count, sangha_count,
                                passage_count, latency_ms, sensitive_category)
         values ($1, $2, 'standard', 2, 1, $3, 10, $4) returning id",
    )
    .bind(tester)
    .bind(question)
    .bind(i32::try_from(passages.len()).unwrap())
    .bind(category)
    .fetch_one(pool)
    .await
    .unwrap();
    let mut payload =
        serde_json::json!({ "mode": "standard", "passage_ids": passages, "summary_pending": true });
    if summary {
        payload["summary"] = serde_json::json!({
            "sentences": [{ "part": "headline", "text": "Talks say practice is not self-improvement.", "cites": [] }]
        });
    }
    let answer: Uuid = sqlx::query_scalar(
        "insert into answer (query_log_id, payload) values ($1, $2) returning id",
    )
    .bind(log)
    .bind(payload)
    .fetch_one(pool)
    .await
    .unwrap();
    AnswerId::from(answer)
}

async fn passages(pool: &PgPool) -> (PassageId, PassageId) {
    let a = talk(pool, Some(ADA), Some(ADA)).await;
    let b = talk(pool, Some(BO), Some(BO)).await;
    consent(pool, ADA, ConsentStatus::Granted).await;
    consent(pool, BO, ConsentStatus::Granted).await;
    (make_ready(pool, a).await, make_ready(pool, b).await)
}

#[tokio::test]
async fn a_pinned_answer_is_found_under_any_spelling_of_its_question() {
    let fx = common::start().await;
    let (pa, pb) = passages(&fx.pool).await;
    let answer = stored(&fx.pool, Some(QUESTION), &[pa, pb], true, "none").await;

    let payload = source(&fx.pool, answer, QUESTION).await.unwrap();
    assert!(!payload.summary_pending, "a pin never waits on a summary");
    pin(&fx.pool, QUESTION, &payload, Some(answer), ACTOR)
        .await
        .unwrap();

    let found = find(
        &fx.pool,
        "what is the difference between practice and trying to improve myself",
    )
    .await
    .unwrap()
    .expect("the pin");
    assert_eq!(found.question_text, QUESTION);
    assert_eq!(found.payload.passage_ids, vec![pa, pb]);
    assert_eq!(found.payload.summary.unwrap().sentences.len(), 1);
    assert!(find(&fx.pool, "What is zazen?").await.unwrap().is_none());

    let rows = list(&fx.pool).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source_answer_id, Some(answer));
    assert_eq!((rows[0].passages, rows[0].sentences), (2, 1));

    // Pinning again replaces the pin rather than adding one.
    pin(&fx.pool, QUESTION, &payload, Some(answer), ACTOR)
        .await
        .unwrap();
    assert_eq!(list(&fx.pool).await.unwrap().len(), 1);

    assert!(unpin(&fx.pool, QUESTION).await.unwrap());
    assert!(find(&fx.pool, QUESTION).await.unwrap().is_none());
    assert!(!unpin(&fx.pool, QUESTION).await.unwrap());
}

#[tokio::test]
async fn only_an_ordinary_summarized_answer_to_the_same_question_can_be_pinned() {
    let fx = common::start().await;
    let (pa, pb) = passages(&fx.pool).await;

    let other = stored(&fx.pool, Some("What is zazen?"), &[pa], true, "none").await;
    assert!(matches!(
        source(&fx.pool, other, QUESTION).await,
        Err(PinError::Mismatch { .. })
    ));

    let unsummarized = stored(&fx.pool, Some(QUESTION), &[pa, pb], false, "none").await;
    assert!(matches!(
        source(&fx.pool, unsummarized, QUESTION).await,
        Err(PinError::Unsuitable(..))
    ));

    let sensitive = stored(&fx.pool, Some(QUESTION), &[pa], true, "distress").await;
    assert!(matches!(
        source(&fx.pool, sensitive, QUESTION).await,
        Err(PinError::Unsuitable(..))
    ));

    let purged = stored(&fx.pool, None, &[pa], true, "none").await;
    assert!(matches!(
        source(&fx.pool, purged, QUESTION).await,
        Err(PinError::NoText(_))
    ));

    let missing = AnswerId::from(Uuid::new_v4());
    assert!(matches!(
        source(&fx.pool, missing, QUESTION).await,
        Err(PinError::NoAnswer(_))
    ));
}

#[tokio::test]
async fn takedown_deletes_the_pins_that_hold_a_removed_passage() {
    let fx = common::start().await;
    let (pa, pb) = passages(&fx.pool).await;
    let both = stored(&fx.pool, Some(QUESTION), &[pa, pb], true, "none").await;
    let payload = source(&fx.pool, both, QUESTION).await.unwrap();
    pin(&fx.pool, QUESTION, &payload, Some(both), ACTOR)
        .await
        .unwrap();
    let bo_only = stored(&fx.pool, Some("What is zazen?"), &[pb], true, "none").await;
    let payload = source(&fx.pool, bo_only, "What is zazen?").await.unwrap();
    pin(&fx.pool, "What is zazen?", &payload, Some(bo_only), ACTOR)
        .await
        .unwrap();

    let report = run(&fx.pool, &Scope::Teacher(ADA.to_owned()), ACTOR)
        .await
        .unwrap();

    assert_eq!(report.pins_removed, 1);
    assert!(find(&fx.pool, QUESTION).await.unwrap().is_none());
    assert!(find(&fx.pool, "What is zazen?").await.unwrap().is_some());
}
