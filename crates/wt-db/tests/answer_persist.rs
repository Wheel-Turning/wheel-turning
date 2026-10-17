//! Saving an answer while the index step replaces passages (October 12, 2026): a passage chosen
//! by retrieval that is gone by the time the answer is stored is left out, instead of failing
//! the save on the `answer_passage` foreign key.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{ADA, consent, make_ready, talk};
use sqlx::types::Uuid;
use wt_core::gate::ConsentStatus;
use wt_core::ids::{PassageId, TesterId};
use wt_db::answer::{NewAnswer, Payload, persist};

#[tokio::test]
async fn a_passage_replaced_before_the_save_is_left_out_of_the_answer() {
    let fx = common::start().await;
    let t = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    let kept = make_ready(&fx.pool, t).await;
    let gone = PassageId::from(Uuid::new_v4());
    let tester: Uuid = sqlx::query_scalar(
        "insert into tester (label, token_hash) values ('t', decode(md5('t'), 'hex')) returning id",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();

    let payload = Payload {
        mode: "standard".into(),
        named_teacher_id: None,
        notice: None,
        notice_count: None,
        themes: Vec::new(),
        summary: None,
        summary_pending: false,
        passage_ids: vec![gone, kept],
        rerank_degraded: false,
        curated: false,
        spans: Vec::new(),
    };
    let answer = persist(
        &fx.pool,
        &NewAnswer {
            tester: TesterId::from(tester),
            query_text: "fixture question",
            payload,
            teacher_count: 1,
            sangha_count: 1,
            thin_coverage: false,
            latency_ms: 10,
            themes_dropped: 0,
            sensitive_category: "none",
            relevance: None,
            triage_degraded: false,
            privacy_mode: "share",
            text_expires_at: None,
            summary_status: wt_core::answer::SummaryStatus::Shown,
            measures: Default::default(),
        },
    )
    .await
    .expect("the save succeeds although one passage is gone");

    let stored: Vec<Uuid> =
        sqlx::query_scalar("select passage_id from answer_passage where answer_id = $1")
            .bind(answer.as_uuid())
            .fetch_all(&fx.pool)
            .await
            .unwrap();
    assert_eq!(stored, vec![kept.as_uuid()]);
    let ids: serde_json::Value =
        sqlx::query_scalar("select payload->'passage_ids' from answer where id = $1")
            .bind(answer.as_uuid())
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(ids, serde_json::json!([kept.as_uuid()]));
    let logged: i32 = sqlx::query_scalar("select passage_count from query_log")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(logged, 1);
}
