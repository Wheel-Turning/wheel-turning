//! Spec 11 PB4.6: `wt report beta` on synthetic data, read as the analyst role. Each figure is
//! checked against rows written here, and the report never carries a question's text.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::Secret;

const PASSWORD: &str = "test-password-0123456789abcdef";

/// Every question below carries this text where it keeps one.
const SECRET: &str = "a private worry about my mother";

async fn analyst(fx: &common::Fx) -> PgPool {
    wt_db::create_login(
        &fx.pool,
        "wt_analyst",
        "wt_analyst_login",
        &Secret::new(PASSWORD),
    )
    .await
    .unwrap();
    let rest = fx.url().expose().split_once('@').unwrap().1;
    let url = Secret::new(format!("postgres://wt_analyst_login:{PASSWORD}@{rest}"));
    wt_db::connect(&url).await.unwrap()
}

async fn tester(pool: &PgPool, label: &str) -> Uuid {
    sqlx::query_scalar(
        "insert into tester (label, token_hash) values ($1, decode(md5($1), 'hex')) returning id",
    )
    .bind(label)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// What one synthetic question records.
#[derive(Default)]
struct Q {
    days_ago: i32,
    cost: Option<f64>,
    first_ms: Option<i32>,
    page_ms: Option<i32>,
    /// `answered`, `purged` (a count-only answer the purge removed) or `acute` (no answer).
    kind: &'static str,
    /// The answer's summary status, when it has one.
    status: Option<&'static str>,
}

/// Logs a question and, unless it is acute, its answer. Returns the answer id when one is kept.
async fn ask(pool: &PgPool, tester: Uuid, q: Q) -> Option<Uuid> {
    let acute = q.kind == "acute";
    let text = (q.kind == "answered").then_some(SECRET);
    let log: Uuid = sqlx::query_scalar(
        "insert into query_log (tester_id, query_text, mode, teacher_count, sangha_count,
                                passage_count, latency_ms, created_at, llm_cost_usd,
                                first_progress_ms, page_ms, sensitive_category, privacy_mode)
         values ($1, $2, 'standard', 2, 1, 3, 10, now() - make_interval(days => $3), $4, $5,
                 $6, $7, $8)
         returning id",
    )
    .bind(tester)
    .bind(text)
    .bind(q.days_ago)
    .bind(q.cost)
    .bind(q.first_ms)
    .bind(q.page_ms)
    .bind(if acute { "acute" } else { "none" })
    .bind(if q.kind == "answered" {
        "share"
    } else {
        "count_only"
    })
    .fetch_one(pool)
    .await
    .unwrap();
    match q.kind {
        "answered" => Some(
            sqlx::query_scalar(
                "insert into answer (query_log_id, payload, summary_status)
                 values ($1, '{\"mode\": \"standard\", \"passage_ids\": []}', $2) returning id",
            )
            .bind(log)
            .bind(q.status)
            .fetch_one(pool)
            .await
            .unwrap(),
        ),
        "purged" => {
            sqlx::query("update query_log set purged_answer_id = gen_random_uuid() where id = $1")
                .bind(log)
                .execute(pool)
                .await
                .unwrap();
            None
        }
        _ => None,
    }
}

async fn click(pool: &PgPool, answer: Option<Uuid>) {
    sqlx::query("insert into dana_event (answer_id, kind) values ($1, 'link_click')")
        .bind(answer)
        .execute(pool)
        .await
        .unwrap();
}

async fn feedback(pool: &PgPool, tester: Uuid, answer: Uuid, kinds: &[&str]) {
    let submission = Uuid::new_v4();
    for kind in kinds {
        let note = (*kind == "note").then_some("the third excerpt was cut off");
        sqlx::query(
            "insert into feedback (answer_id, tester_id, target, kind, note, submission_id)
             values ($1, $2, 'answer', $3, $4, $5)",
        )
        .bind(answer)
        .bind(tester)
        .bind(kind)
        .bind(note)
        .bind(submission)
        .execute(pool)
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn the_beta_report_counts_each_figure_and_never_prints_a_question() {
    let fx = common::start().await;
    let pool = &fx.pool;
    let (a, b, c, eval) = (
        tester(pool, "ana").await,
        tester(pool, "ben").await,
        tester(pool, "cy").await,
        tester(pool, "eval").await,
    );

    // Ana: three answered questions this week and one two weeks ago, so she returns.
    let mut answers = Vec::new();
    for (cost, first, page, status) in [
        (0.01, 100, 1000, "shown"),
        (0.02, 200, 2000, "shown"),
        (0.03, 300, 3000, "timeout"),
    ] {
        let q = Q {
            kind: "answered",
            status: Some(status),
            cost: Some(cost),
            first_ms: Some(first),
            page_ms: Some(page),
            ..Q::default()
        };
        answers.push(ask(pool, a, q).await.unwrap());
    }
    let old = Q {
        kind: "answered",
        days_ago: 14,
        cost: Some(9.0),
        page_ms: Some(90_000),
        ..Q::default()
    };
    ask(pool, a, old).await;
    // Ben: a count-only question whose text and answer the purge removed, asked without the
    // progress stream.
    let purged = Q {
        kind: "purged",
        cost: Some(0.04),
        page_ms: Some(4000),
        ..Q::default()
    };
    ask(pool, b, purged).await;
    // Cy: an acute question, counts only, no answer.
    ask(
        pool,
        c,
        Q {
            kind: "acute",
            ..Q::default()
        },
    )
    .await;
    // An evaluation run is left out of every figure.
    let e = Q {
        kind: "answered",
        cost: Some(5.0),
        first_ms: Some(50_000),
        page_ms: Some(50_000),
        ..Q::default()
    };
    let eval_answer = ask(pool, eval, e).await.unwrap();
    feedback(pool, eval, eval_answer, &["good"]).await;
    click(pool, Some(eval_answer)).await;

    // Two clicks on Ana's first answer, one on an answer purged since.
    click(pool, Some(answers[0])).await;
    click(pool, Some(answers[0])).await;
    click(pool, None).await;
    feedback(pool, a, answers[0], &["good"]).await;
    feedback(pool, a, answers[1], &["not_relevant", "unclear", "note"]).await;

    let analyst = analyst(&fx).await;
    assert!(
        !wt_db::report::reads_query_log(&analyst).await.unwrap(),
        "the report runs as a role that cannot read questions"
    );
    let since = chrono::Utc::now() - chrono::Duration::days(7);
    let r = wt_db::report::beta(&analyst, since).await.unwrap();

    assert_eq!(r.testers.active, 3);
    assert_eq!(r.testers.returning, 1);
    assert_eq!(r.questions.asked, 5);
    assert_eq!(r.questions.answered, 4, "the purged answer still counts");
    assert_eq!(r.questions.median_per_active_tester, 1.0);

    assert_eq!(r.feedback.submissions, 2);
    assert_eq!(r.feedback.helpful, 1);
    assert_eq!(r.feedback.helpful_share, 0.5);
    let reasons: Vec<(&str, i64)> = r
        .feedback
        .reasons
        .iter()
        .map(|(k, v)| (k.as_str(), *v))
        .collect();
    assert_eq!(
        reasons,
        [
            ("good", 1),
            ("not_relevant", 1),
            ("note", 1),
            ("unclear", 1)
        ]
    );

    assert_eq!(r.dana.link_clicks, 3);
    assert_eq!(r.dana.clicks_per_answer, 0.75);
    assert_eq!(r.dana.testers_with_click, 1);
    assert!((r.dana.share_of_testers_with_click - 1.0 / 3.0).abs() < 1e-9);

    // Two shown and a timeout among Ana's; Ben's status went with his purged answer.
    assert_eq!(r.summaries.attempted, 3);
    assert!((r.summaries.shown_share - 2.0 / 3.0).abs() < 1e-9);
    let by: Vec<(&str, i64, f64)> = r
        .summaries
        .by_status
        .iter()
        .map(|(k, v)| (k.as_str(), v.answers, v.share))
        .collect();
    assert_eq!(
        by,
        [
            ("shown", 2, 0.5),
            ("timeout", 1, 0.25),
            ("unrecorded", 1, 0.25)
        ]
    );

    assert_eq!(r.cost.answers, 4);
    assert!((r.cost.mean_usd - 0.025).abs() < 1e-9);
    assert!((r.cost.p95_usd - 0.04).abs() < 1e-9);

    assert_eq!(r.speed.streamed, 3);
    assert_eq!(r.speed.first_progress_p50_ms, 200.0);
    assert_eq!(r.speed.first_progress_p95_ms, 300.0);
    assert_eq!(r.speed.pages, 4);
    assert_eq!(r.speed.page_p50_ms, 2000.0);
    assert_eq!(r.speed.page_p95_ms, 4000.0);

    let printed = serde_json::to_string(&r).unwrap();
    assert!(!printed.contains(SECRET), "no question text: {printed}");
    assert!(!printed.contains("cut off"), "no feedback note: {printed}");
    for id in [a, b, c] {
        assert!(
            !printed.contains(&id.to_string()),
            "no tester id: {printed}"
        );
    }
}

#[tokio::test]
async fn the_beta_report_runs_on_an_empty_beta() {
    let fx = common::start().await;
    let analyst = analyst(&fx).await;
    let since = chrono::Utc::now() - chrono::Duration::days(7);
    let r = wt_db::report::beta(&analyst, since).await.unwrap();
    assert_eq!(r.testers.active, 0);
    assert_eq!(r.cost.mean_usd, 0.0);
    assert_eq!(r.dana.clicks_per_answer, 0.0);
}
