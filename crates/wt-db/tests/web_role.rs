//! Spec 02, roles: every query the web service makes runs as `wt_web`. The owner's alpha
//! connected as a superuser, which hid the answer page, the feedback and dana routes and the
//! session check reading tables `wt_web` has no grant on (found on AWS, October 13, 2026). Each
//! test here logs in through a `wt_web` login and calls the library functions `wt-api` calls.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{ADA, BO, consent, make_ready, talk};
use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::Secret;
use wt_core::gate::ConsentStatus;
use wt_core::ids::{AnswerId, PassageId, TalkId, TesterId};
use wt_db::answer::{self, DanaEvent, FeedbackOutcome, FeedbackTarget, NewAnswer, Payload};

const PASSWORD: &str = "test-password-0123456789abcdef";

async fn web_login(fx: &common::Fx) -> PgPool {
    wt_db::create_login(&fx.pool, "wt_web", "wt_web_login", &Secret::new(PASSWORD))
        .await
        .unwrap();
    let rest = fx.url().expose().split_once('@').unwrap().1;
    let url = Secret::new(format!("postgres://wt_web_login:{PASSWORD}@{rest}"));
    wt_db::connect(&url).await.unwrap()
}

async fn tester(pool: &PgPool, label: &str) -> TesterId {
    wt_db::tester::invite(pool, label, label.as_bytes())
        .await
        .unwrap()
}

async fn served(pool: &PgPool, teacher: &str) -> (TalkId, PassageId) {
    let t = talk(pool, Some(teacher), Some(teacher)).await;
    consent(pool, teacher, ConsentStatus::Granted).await;
    let p = make_ready(pool, t).await;
    (t, p)
}

fn new_answer(tester: TesterId, passages: &[PassageId]) -> NewAnswer<'static> {
    let payload: Payload = serde_json::from_value(serde_json::json!({
        "mode": "standard",
        "themes": [],
        "passage_ids": passages,
    }))
    .unwrap();
    NewAnswer {
        tester,
        query_text: "How do I sit with restlessness?",
        payload,
        teacher_count: 1,
        sangha_count: 1,
        thin_coverage: false,
        latency_ms: 10,
        themes_dropped: 0,
        sensitive_category: "none",
        relevance: Some("practice"),
        triage_degraded: false,
        privacy_mode: "share",
        text_expires_at: None,
    }
}

#[tokio::test]
async fn the_web_role_asks_shows_and_takes_feedback_on_its_testers_own_answers() {
    let fx = common::start().await;
    let (t, p) = served(&fx.pool, ADA).await;
    let alice = tester(&fx.pool, "alice").await;
    let bob = tester(&fx.pool, "bob").await;
    let web = web_login(&fx).await;

    // The session check notes the visit.
    assert!(wt_db::tester::active(&web, alice).await.unwrap());
    assert!(
        wt_db::tester::by_token_hash(&web, b"alice")
            .await
            .unwrap()
            .is_some()
    );

    // Asking: named-teacher detection, the stored answer, and the logs without an answer.
    assert!(!answer::aliases(&web).await.unwrap().is_empty());
    let a = answer::persist(&web, &new_answer(alice, &[p]))
        .await
        .unwrap();
    answer::log_counts_only(&web, alice, "acute", 10, None, false)
        .await
        .unwrap();
    answer::log_unrelated(
        &web,
        &answer::Unanswered {
            tester: alice,
            query_text: "What is the weather?",
            privacy_mode: "share",
            text_expires_at: None,
            latency_ms: 10,
        },
    )
    .await
    .unwrap();

    // The answer page: the owner sees it, anyone else gets nothing.
    let stored = answer::load(&web, a, alice).await.unwrap().unwrap();
    assert_eq!(stored.query_text, "How do I sit with restlessness?");
    assert_eq!(stored.payload.passage_ids, vec![p]);
    assert!(answer::load(&web, a, bob).await.unwrap().is_none());
    assert!(!wt_db::privacy::was_purged(&web, a, alice).await.unwrap());
    assert!(!answer::passages(&web, &[p]).await.unwrap().is_empty());
    answer::windows(&web, &[p]).await.unwrap();
    answer::excerpt_rows(&web, &[p]).await.unwrap();
    answer::cited_works(&web, &[p]).await.unwrap();
    wt_db::questions::for_passages(&web, &[p]).await.unwrap();
    assert!(answer::talk(&web, t).await.unwrap().is_some());
    wt_db::questions::for_talk(&web, t, 1000).await.unwrap();

    // Feedback on an excerpt shown, and not on one that was not.
    let (_, other) = served(&fx.pool, BO).await;
    let on = |passage| FeedbackTarget::Excerpt(passage);
    assert_eq!(
        answer::submit_feedback(&web, a, alice, on(p), &["good"], None)
            .await
            .unwrap(),
        FeedbackOutcome::Recorded
    );
    assert_eq!(
        answer::submit_feedback(&web, a, alice, on(other), &["good"], None)
            .await
            .unwrap(),
        FeedbackOutcome::NotFound
    );
    assert_eq!(
        answer::submit_feedback(&web, a, bob, FeedbackTarget::Summary, &["good"], None)
            .await
            .unwrap(),
        FeedbackOutcome::NotFound
    );

    // Flag and dana events, only on the tester's own answer.
    assert!(answer::flag_query(&web, a, alice).await.unwrap());
    assert!(!answer::flag_query(&web, a, bob).await.unwrap());
    assert!(
        answer::dana_event(&web, a, alice, DanaEvent::PanelOpen)
            .await
            .unwrap()
    );
    assert!(
        !answer::dana_event(&web, a, bob, DanaEvent::PanelOpen)
            .await
            .unwrap()
    );
    assert!(answer::give_site(&web, a, alice).await.unwrap());
    assert!(!answer::give_site(&web, a, bob).await.unwrap());
    let sangha: Uuid = sqlx::query_scalar("select id from sangha limit 1")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    answer::give(&web, a, alice, sangha.into()).await.unwrap();
    answer::dana_sanghas(&web, &[sangha.into()]).await.unwrap();

    // Privacy: the mode and deleting one's own questions.
    wt_db::privacy::choose(&web, alice, "count_only")
        .await
        .unwrap();
    wt_db::privacy::choice(&web, alice).await.unwrap();
    wt_db::privacy::delete_questions(&web, alice).await.unwrap();
    assert!(answer::load(&web, a, alice).await.unwrap().is_none());
}

#[tokio::test]
async fn the_web_role_still_cannot_read_the_query_log_or_answers() {
    let fx = common::start().await;
    let (_, p) = served(&fx.pool, ADA).await;
    let alice = tester(&fx.pool, "alice").await;
    let web = web_login(&fx).await;
    let a: AnswerId = answer::persist(&web, &new_answer(alice, &[p]))
        .await
        .unwrap();
    assert!(answer::load(&web, a, alice).await.unwrap().is_some());

    for table in ["query_log", "answer", "answer_passage"] {
        let read = sqlx::query(sqlx::AssertSqlSafe(format!("select count(*) from {table}")))
            .execute(&web)
            .await;
        assert!(read.is_err(), "wt_web must not select from {table}");
    }
    let rename = sqlx::query("update tester set label = 'renamed'")
        .execute(&web)
        .await;
    assert!(rename.is_err(), "wt_web may update last_seen_at only");
}
