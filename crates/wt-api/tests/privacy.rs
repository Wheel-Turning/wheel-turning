//! Spec 06 acceptance for question privacy: the switch, Sec-GPC, personal details removed in
//! keep mode, the purge with its exceptions, and deleting a tester's questions.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use sqlx::PgPool;
use sqlx::types::Uuid;
use tower::ServiceExt;
use wt_api::{AppState, Site, router, session};
use wt_db::test_support::TestDb;

struct Fx {
    _db: TestDb,
    pool: PgPool,
    app: Router,
}

async fn start() -> Fx {
    let db = TestDb::start()
        .await
        .expect("docker must be running for database tests");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();
    let app = router(AppState {
        pool: pool.clone(),
        site: Arc::new(Site::with_fakes("fake-embed")),
    });
    Fx { _db: db, pool, app }
}

struct Reply {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

async fn send(app: &Router, req: Request<Body>) -> Reply {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn get(path: &str, cookie: &str) -> Request<Body> {
    Request::get(path)
        .header(header::COOKIE, format!("wt_session={cookie}"))
        .body(Body::empty())
        .unwrap()
}

fn post(path: &str, cookie: &str, form: &str, gpc: bool) -> Request<Body> {
    let mut r = Request::post(path)
        .header(header::COOKIE, format!("wt_session={cookie}"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if gpc {
        r = r.header("sec-gpc", "1");
    }
    r.body(Body::from(form.to_owned())).unwrap()
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            b' ' => "+".to_owned(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

struct Tester {
    id: Uuid,
    cookie: String,
    csrf: String,
}

async fn join(fx: &Fx, label: &str) -> Tester {
    let token = session::new_token().unwrap();
    let id = wt_db::tester::invite(&fx.pool, label, &session::token_hash(&token))
        .await
        .unwrap();
    let r = send(
        &fx.app,
        Request::get(format!("/join/{token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let set = r.headers[header::SET_COOKIE].to_str().unwrap().to_owned();
    let cookie = set
        .split(';')
        .next()
        .unwrap()
        .trim_start_matches("wt_session=")
        .to_owned();
    let home = send(&fx.app, get("/", &cookie)).await;
    let at = home.body.find("name=\"csrf\" value=\"").unwrap() + "name=\"csrf\" value=\"".len();
    let csrf = home.body[at..].split('"').next().unwrap().to_owned();
    Tester {
        id: id.as_uuid(),
        cookie,
        csrf,
    }
}

/// Asks and returns the permalink.
async fn ask(fx: &Fx, t: &Tester, question: &str, gpc: bool) -> String {
    let r = send(
        &fx.app,
        post(
            "/ask",
            &t.cookie,
            &format!("q={}&csrf={}", encode(question), encode(&t.csrf)),
            gpc,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    r.headers[header::LOCATION].to_str().unwrap().to_owned()
}

async fn choose(fx: &Fx, t: &Tester, keep: bool) {
    let form = format!(
        "csrf={}&back=about{}",
        encode(&t.csrf),
        if keep { "&keep=1" } else { "" }
    );
    let r = send(&fx.app, post("/about/privacy", &t.cookie, &form, false)).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.headers[header::LOCATION], "/about?saved=1#privacy");
}

async fn last_log(pool: &PgPool, tester: Uuid) -> (Option<String>, String) {
    sqlx::query_as(
        "select query_text, privacy_mode from query_log where tester_id = $1
         order by created_at desc limit 1",
    )
    .bind(tester)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn purge(pool: &PgPool) -> i32 {
    wt_db::privacy::purge(pool, 180).await.unwrap()
}

#[tokio::test]
async fn the_switch_shows_on_home_and_about_and_applies_to_the_next_question() {
    let fx = start().await;
    let t = join(&fx, "owner").await;
    for path in ["/", "/about"] {
        let page = send(&fx.app, get(path, &t.cookie)).await;
        assert!(
            page.body
                .contains("Keep my questions to help improve Wheel Turning."),
            "{path}"
        );
        assert!(
            page.body.contains(" checked"),
            "{path}: share is the default"
        );
    }
    ask(&fx, &t, "How do I sit with anger?", false).await;
    assert_eq!(last_log(&fx.pool, t.id).await.1, "share");

    choose(&fx, &t, false).await;
    let (mode, chosen): (String, bool) =
        sqlx::query_as("select privacy_mode, privacy_chosen from tester where id = $1")
            .bind(t.id)
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!((mode.as_str(), chosen), ("count_only", true));
    let about = send(&fx.app, get("/about?saved=1", &t.cookie)).await;
    assert!(
        about
            .body
            .contains("Saved. This applies to your next question.")
    );
    assert!(about.body.contains("Your question is kept for 24 hours"));
    ask(&fx, &t, "How do I sit with fear?", false).await;
    assert_eq!(last_log(&fx.pool, t.id).await.1, "count_only");
}

#[tokio::test]
async fn keep_mode_stores_the_question_without_personal_details() {
    let fx = start().await;
    let t = join(&fx, "owner").await;
    ask(
        &fx,
        &t,
        "I am jo@example.org, call 415-555-0123, see https://example.org/me, id 1234567890",
        false,
    )
    .await;
    let (text, mode) = last_log(&fx.pool, t.id).await;
    assert_eq!(mode, "share");
    assert_eq!(
        text.as_deref(),
        Some("I am [removed], call [removed], see [removed] id [removed]")
    );
}

#[tokio::test]
async fn gpc_means_count_only_until_the_tester_chooses() {
    let fx = start().await;
    let t = join(&fx, "owner").await;
    ask(&fx, &t, "How do I sit with doubt?", true).await;
    assert_eq!(last_log(&fx.pool, t.id).await.1, "count_only");
    choose(&fx, &t, true).await;
    ask(&fx, &t, "How do I sit with doubt again?", true).await;
    assert_eq!(last_log(&fx.pool, t.id).await.1, "share");
}

#[tokio::test]
async fn an_expired_question_loses_its_text_and_answer_and_keeps_its_counts_and_clicks() {
    let fx = start().await;
    let t = join(&fx, "owner").await;
    choose(&fx, &t, false).await;
    let permalink = ask(&fx, &t, "How do I sit with grief?", false).await;
    let answer: Uuid = permalink.trim_start_matches("/a/").parse().unwrap();
    sqlx::query("insert into dana_event (answer_id, kind) values ($1, 'link_click')")
        .bind(answer)
        .execute(&fx.pool)
        .await
        .unwrap();
    let clicks = "select count(*) from dana_event where kind = 'link_click'
                  and created_at::date = current_date";
    let before: i64 = sqlx::query_scalar(clicks)
        .fetch_one(&fx.pool)
        .await
        .unwrap();

    assert_eq!(purge(&fx.pool).await, 0, "nothing has expired yet");
    sqlx::query("update query_log set text_expires_at = now() - interval '1 minute'")
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(purge(&fx.pool).await, 1);

    let (text, mode) = last_log(&fx.pool, t.id).await;
    assert_eq!((text, mode.as_str()), (None, "count_only"));
    let (answers, logs): (i64, i64) =
        sqlx::query_as("select (select count(*) from answer), (select count(*) from query_log)")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!((answers, logs), (0, 1), "the counts stay");
    let after: i64 = sqlx::query_scalar(clicks)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(after, before, "the daily click count does not change");
    let orphan: Option<Uuid> = sqlx::query_scalar("select answer_id from dana_event")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(orphan, None);

    let page = send(&fx.app, get(&permalink, &t.cookie)).await;
    assert_eq!(page.status, StatusCode::GONE);
    assert!(page.body.contains("This question was not kept."));
    let other = join(&fx, "someone else").await;
    let theirs = send(&fx.app, get(&permalink, &other.cookie)).await;
    assert_eq!(
        theirs.status,
        StatusCode::NOT_FOUND,
        "only its asker learns it existed"
    );
}

#[tokio::test]
async fn a_flagged_question_survives_the_purge_in_both_modes() {
    let fx = start().await;
    let t = join(&fx, "owner").await;
    for keep in [true, false] {
        choose(&fx, &t, keep).await;
        let permalink = ask(&fx, &t, "How do I sit with shame?", false).await;
        let r = send(
            &fx.app,
            post(
                &format!("{permalink}/feedback"),
                &t.cookie,
                &format!("kind=flag_query&csrf={}", encode(&t.csrf)),
                false,
            ),
        )
        .await;
        assert_eq!(r.status, StatusCode::SEE_OTHER);
    }
    sqlx::query("update query_log set text_expires_at = now() - interval '1 minute'")
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(purge(&fx.pool).await, 0);
    let kept: i64 =
        sqlx::query_scalar("select count(*) from query_log where query_text is not null")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(kept, 2);
    // After the full retention period, flagged text goes too.
    sqlx::query("update query_log set created_at = now() - interval '181 days'")
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(purge(&fx.pool).await, 2);
}

#[tokio::test]
async fn deleting_my_data_asks_first_and_touches_no_one_else() {
    let fx = start().await;
    let me = join(&fx, "me").await;
    let them = join(&fx, "them").await;
    for t in [&me, &them] {
        let permalink = ask(&fx, t, "How do I sit with boredom?", false).await;
        let answer: Uuid = permalink.trim_start_matches("/a/").parse().unwrap();
        send(
            &fx.app,
            post(
                &format!("{permalink}/feedback"),
                &t.cookie,
                &format!("kind=good&csrf={}", encode(&t.csrf)),
                false,
            ),
        )
        .await;
        sqlx::query("insert into dana_event (answer_id, kind) values ($1, 'panel_open')")
            .bind(answer)
            .execute(&fx.pool)
            .await
            .unwrap();
    }
    let rows = |id: Uuid| {
        let pool = fx.pool.clone();
        async move {
            let r: (i64, i64, i64, i64) = sqlx::query_as(
                "select (select count(*) from query_log where tester_id = $1),
                        (select count(*) from answer a join query_log q on q.id = a.query_log_id
                          where q.tester_id = $1),
                        (select count(*) from feedback where tester_id = $1),
                        (select count(*) from dana_event d join answer a on a.id = d.answer_id
                          join query_log q on q.id = a.query_log_id where q.tester_id = $1)",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
            r
        }
    };
    assert_eq!(rows(me.id).await, (1, 1, 1, 1));

    let refused = send(
        &fx.app,
        post("/about/delete-my-data", &me.cookie, "csrf=wrong", false),
    )
    .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let confirm = send(
        &fx.app,
        post(
            "/about/delete-my-data",
            &me.cookie,
            &format!("csrf={}", encode(&me.csrf)),
            false,
        ),
    )
    .await;
    assert_eq!(confirm.status, StatusCode::OK);
    assert!(confirm.body.contains("It cannot be undone."));
    assert!(confirm.body.contains("Delete everything") && confirm.body.contains("Cancel"));
    assert_eq!(
        rows(me.id).await,
        (1, 1, 1, 1),
        "nothing goes before the confirmation"
    );

    let done = send(
        &fx.app,
        post(
            "/about/delete-my-data",
            &me.cookie,
            &format!("csrf={}&confirm=yes", encode(&me.csrf)),
            false,
        ),
    )
    .await;
    assert_eq!(done.status, StatusCode::SEE_OTHER);
    assert_eq!(rows(me.id).await, (0, 0, 0, 0));
    assert_eq!(rows(them.id).await, (1, 1, 1, 1));
    let leftover: i64 = sqlx::query_scalar("select count(*) from dana_event")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(leftover, 1);
}
