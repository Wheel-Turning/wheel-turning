//! Spec 07, runbook procedure 4: a blocking quality failure quarantines a ready talk, which
//! stops serving and keeps its text; release re-checks it and returns it to serving.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{ACTOR, ADA, consent, make_ready, servable, state, talk};
use wt_core::gate::ConsentStatus;
use wt_db::quarantine::{QuarantineError, quarantine, release};

#[tokio::test]
async fn quarantine_stops_serving_and_release_rechecks_before_serving_again() {
    let fx = common::start().await;
    let t = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    make_ready(&fx.pool, t).await;
    assert_eq!(servable(&fx.pool).await, 1);

    quarantine(&fx.pool, t, "garbled passage text", ACTOR)
        .await
        .unwrap();
    assert_eq!(state(&fx.pool, t).await, "quarantined");
    assert_eq!(servable(&fx.pool).await, 0);
    let kept: i64 = sqlx::query_scalar("select count(*) from passage")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(kept, 1, "quarantine deletes nothing");
    assert!(matches!(
        quarantine(&fx.pool, t, "again", ACTOR).await,
        Err(QuarantineError::Refused(_))
    ));

    // The re-check fails while consent is no longer granted.
    consent(&fx.pool, ADA, ConsentStatus::Requested).await;
    let err = release(&fx.pool, t, "repaired", ACTOR).await.unwrap_err();
    assert!(err.to_string().contains("re-check"), "{err}");
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;

    release(&fx.pool, t, "repaired the passage; re-check passed", ACTOR)
        .await
        .unwrap();
    assert_eq!(state(&fx.pool, t).await, "ready");
    assert_eq!(servable(&fx.pool).await, 1);
    let logged: i64 = sqlx::query_scalar(
        "select count(*) from correction_log where entity = 'talk' and field = 'state'
           and '\"quarantined\"'::jsonb in (old_value, new_value)",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(logged, 2);
}

#[tokio::test]
async fn release_refuses_a_talk_the_pipeline_quarantined() {
    let fx = common::start().await;
    let t = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    sqlx::query("update talk set state = 'quarantined', quarantine_reason = 'held' where id = $1")
        .bind(t.as_uuid())
        .execute(&fx.pool)
        .await
        .unwrap();
    let err = release(&fx.pool, t, "x", ACTOR).await.unwrap_err();
    assert!(err.to_string().contains("pipeline retry"), "{err}");
}
