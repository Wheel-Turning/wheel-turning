//! Spec 07, Usage report: `wt report usage` on synthetic data, read as the analyst role, and the
//! page view writes and purge as the web role. Each figure is checked against rows written here,
//! and the report never carries a question's text, a feedback note or a tester's id.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::Secret;
use wt_core::ids::TesterId;

const PASSWORD: &str = "test-password-0123456789abcdef";

/// Every question below carries this text.
const SECRET: &str = "a private worry about my mother";

async fn login(fx: &common::Fx, role: &str) -> PgPool {
    let user = format!("{role}_login");
    wt_db::create_login(&fx.pool, role, &user, &Secret::new(PASSWORD))
        .await
        .unwrap();
    let rest = fx.url().expose().split_once('@').unwrap().1;
    let url = Secret::new(format!("postgres://{user}:{PASSWORD}@{rest}"));
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

/// A page view `minutes` after the start of the UTC day `days_ago` days back.
async fn view(pool: &PgPool, tester: Uuid, route: &str, status: i16, days_ago: i32, minutes: i32) {
    sqlx::query(
        "insert into page_view (tester_id, method, route, status, ms, created_at)
         values ($1, 'GET', $2, $3, 40,
                 date_trunc('day', now()) - make_interval(days => $4, mins => -$5))",
    )
    .bind(tester)
    .bind(route)
    .bind(status)
    .bind(days_ago)
    .bind(minutes)
    .execute(pool)
    .await
    .unwrap();
}

/// A question, answered, `minutes` after the start of the UTC day `days_ago` days back.
async fn ask(pool: &PgPool, tester: Uuid, days_ago: i32, minutes: i32, flagged: bool) -> Uuid {
    let log: Uuid = sqlx::query_scalar(
        "insert into query_log (tester_id, query_text, mode, teacher_count, sangha_count,
                                passage_count, latency_ms, flagged, created_at)
         values ($1, $2, 'standard', 2, 1, 3, 10, $3,
                 date_trunc('day', now()) - make_interval(days => $4, mins => -$5))
         returning id",
    )
    .bind(tester)
    .bind(SECRET)
    .bind(flagged)
    .bind(days_ago)
    .bind(minutes)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query_scalar(
        "insert into answer (query_log_id, payload)
         values ($1, '{\"mode\": \"standard\", \"passage_ids\": []}') returning id",
    )
    .bind(log)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn the_usage_report_counts_each_tester_and_never_prints_a_question() {
    let fx = common::start().await;
    let pool = &fx.pool;
    let (ana, ben, cy, eval) = (
        tester(pool, "ana").await,
        tester(pool, "ben").await,
        tester(pool, "cy").await,
        tester(pool, "eval").await,
    );

    // Ana, two days ago: two visits (a 40-minute gap), one server error, two questions, one of
    // them flagged, a feedback note and a dana click. A page view 40 days ago is out of range.
    view(pool, ana, "/", 200, 2, 600).await;
    view(pool, ana, "/a/{answer}", 200, 2, 610).await;
    view(pool, ana, "/a/{answer}", 503, 2, 650).await;
    view(pool, ana, "/", 200, 40, 600).await;
    let first = ask(pool, ana, 2, 605, false).await;
    ask(pool, ana, 2, 615, true).await;
    sqlx::query(
        "insert into feedback (answer_id, tester_id, target, kind, note)
         values ($1, $2, 'answer', 'note', 'the third excerpt was cut off')",
    )
    .bind(first)
    .bind(ana)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("insert into dana_event (answer_id, kind) values ($1, 'link_click')")
        .bind(first)
        .execute(pool)
        .await
        .unwrap();

    // Ben opened the invite before page views were kept and asked one question yesterday.
    sqlx::query("update tester set last_seen_at = now() where id = $1")
        .bind(ben)
        .execute(pool)
        .await
        .unwrap();
    ask(pool, ben, 1, 600, false).await;

    // Cy never opened the invite. The evaluation run is left out of every figure.
    view(pool, eval, "/", 200, 2, 600).await;
    ask(pool, eval, 2, 600, false).await;

    let analyst = login(&fx, "wt_analyst").await;
    assert!(!wt_db::report::reads_query_log(&analyst).await.unwrap());
    let since = chrono::Utc::now() - chrono::Duration::days(7);
    let r = wt_db::report::usage(&analyst, since).await.unwrap();

    let labels: Vec<&str> = r.testers.iter().map(|t| t.label.as_str()).collect();
    assert_eq!(
        labels,
        ["ana", "ben", "cy"],
        "eval is left out, cy is listed"
    );

    let a = &r.testers[0];
    assert!(a.opened);
    assert_eq!(
        (a.page_views, a.visits, a.days_active),
        (3, 2, 1),
        "ana's views"
    );
    assert_eq!((a.questions, a.answered, a.flagged), (2, 2, 1));
    assert_eq!((a.feedback, a.dana_clicks), (1, 1));
    assert!(a.first_view < a.last_view);

    let b = &r.testers[1];
    assert!(b.opened, "last seen counts as opened");
    assert_eq!(
        (b.page_views, b.visits, b.days_active, b.questions),
        (0, 0, 1, 1)
    );
    assert_eq!(b.first_view, None);

    let c = &r.testers[2];
    assert!(!c.opened);
    assert_eq!((c.page_views, c.questions, c.days_active), (0, 0, 0));

    let routes: Vec<(&str, &str, i32, i32, i32)> = r
        .routes
        .iter()
        .map(|x| {
            (
                x.method.as_str(),
                x.route.as_str(),
                x.page_views,
                x.testers,
                x.errors,
            )
        })
        .collect();
    assert_eq!(
        routes,
        [("GET", "/a/{answer}", 2, 1, 1), ("GET", "/", 1, 1, 0)]
    );

    let days: Vec<(i32, i32, i32)> = r
        .days
        .iter()
        .map(|d| (d.active_testers, d.page_views, d.questions))
        .collect();
    assert_eq!(days, [(1, 3, 2), (1, 0, 1)]);

    let printed = serde_json::to_string(&r).unwrap();
    assert!(!printed.contains(SECRET), "no question text: {printed}");
    assert!(!printed.contains("cut off"), "no feedback note: {printed}");
    for id in [ana, ben, cy] {
        assert!(
            !printed.contains(&id.to_string()),
            "no tester id: {printed}"
        );
    }
}

#[tokio::test]
async fn the_web_role_writes_and_purges_page_views_but_cannot_read_them() {
    let fx = common::start().await;
    let ana = tester(&fx.pool, "ana").await;
    view(&fx.pool, ana, "/", 200, 40, 0).await;
    view(&fx.pool, ana, "/", 200, 29, 0).await;

    let web = login(&fx, "wt_web").await;
    let fresh = wt_db::page_view::PageView {
        tester: TesterId::from_uuid(ana),
        method: "GET".to_owned(),
        route: "/about".to_owned(),
        status: 200,
        ms: 12,
        at: chrono::Utc::now(),
    };
    wt_db::page_view::record(&web, &fresh).await.unwrap();
    assert!(
        sqlx::query("select 1 from page_view")
            .fetch_all(&web)
            .await
            .is_err(),
        "the web role inserts only"
    );

    assert_eq!(wt_db::page_view::purge(&web, 30).await.unwrap(), 1);
    let left: Vec<String> = sqlx::query_scalar("select route from page_view order by created_at")
        .fetch_all(&fx.pool)
        .await
        .unwrap();
    assert_eq!(
        left,
        ["/", "/about"],
        "the 29-day-old view and the new one stay"
    );
}
