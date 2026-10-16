//! Spec 06 and Spec 08 acceptance for the pages that need no session (Rights and removal,
//! Accessibility, the icons and the menu script), the structure every page shares, and the
//! per-tester ask limit. Synthetic sources only.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use sqlx::PgPool;
use tower::ServiceExt;
use wt_api::mail::{FakeMailer, UnavailableMailer};
use wt_api::{AppState, Limits, Site, router, session};
use wt_db::test_support::TestDb;

struct Fx {
    _db: TestDb,
    pool: PgPool,
    app: Router,
}

async fn start_with(site: Site) -> Fx {
    let db = TestDb::start()
        .await
        .expect("docker must be running for database tests");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();
    let app = router(AppState {
        pool: pool.clone(),
        site: Arc::new(site),
    });
    Fx { _db: db, pool, app }
}

async fn start() -> Fx {
    start_with(Site::with_fakes("fake-embed")).await
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

fn get(path: &str, cookie: Option<&str>) -> Request<Body> {
    let mut r = Request::get(path);
    if let Some(c) = cookie {
        r = r.header(header::COOKIE, format!("wt_session={c}"));
    }
    r.body(Body::empty()).unwrap()
}

fn post(path: &str, cookie: Option<&str>, form: &str) -> Request<Body> {
    let mut r = Request::post(path)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header("x-forwarded-for", "203.0.113.9");
    if let Some(c) = cookie {
        r = r.header(header::COOKIE, format!("wt_session={c}"));
    }
    r.body(Body::from(form.to_owned())).unwrap()
}

async fn join(fx: &Fx, label: &str) -> String {
    let token = session::new_token().unwrap();
    wt_db::tester::invite(&fx.pool, label, &session::token_hash(&token))
        .await
        .unwrap();
    let r = send(&fx.app, get(&format!("/join/{token}"), None)).await;
    let set = r.headers[header::SET_COOKIE].to_str().unwrap().to_owned();
    set.split(';')
        .next()
        .unwrap()
        .trim_start_matches("wt_session=")
        .to_owned()
}

fn csrf(body: &str) -> String {
    let at = body.find("name=\"csrf\" value=\"").unwrap() + "name=\"csrf\" value=\"".len();
    body[at..].split('"').next().unwrap().to_owned()
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

async fn count(pool: &PgPool, table: &'static str) -> i64 {
    let sql = match table {
        "takedown_request" => "select count(*) from takedown_request",
        _ => "select count(*) from accessibility_report",
    };
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

/// Three sources: a mandate with a request sent, a permission, and one the holder declined.
async fn sources(pool: &PgPool) {
    sqlx::query(
        "insert into source_policy (key, publisher, url_prefixes, basis, kind) values
           ('mand', 'Mandate Fixture Press', '{https://m.invalid/}', 'mandate', 'texts'),
           ('perm', 'Permission Fixture Zendo', '{https://p.invalid/}', 'permission', 'talks'),
           ('gone', 'Declined Fixture Archive', '{https://g.invalid/}', 'mandate', 'texts')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("update source_policy set enabled = false where key = 'gone'")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into permission_request (source_key, recipient, channel, sent_on, status)
         values ('mand', 'https://m.invalid/contact', 'form', '2026-10-05', 'sent')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into rights_event (source_key, event, actor, created_at) values
           ('perm', 'permission_granted', 'test', '2026-10-06T12:00:00Z'),
           ('gone', 'permission_declined', 'test', '2026-10-07T12:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn the_rights_page_needs_no_session_and_states_each_source_in_plain_words() {
    let fx = start().await;
    sources(&fx.pool).await;
    let r = send(&fx.app, get("/rights", None)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.headers["content-security-policy"],
        "default-src 'self'; frame-ancestors 'none'"
    );
    assert!(
        r.body
            .contains("If you hold rights in any talk or text shown here")
    );
    assert!(
        r.body
            .contains("Within one business day of your confirming it")
    );
    let line = |name: &str| {
        let at = r.body.find(name).unwrap();
        r.body[at..r.body[at..].find("</li>").unwrap() + at].to_owned()
    };
    let mandate = line("Mandate Fixture Press");
    assert!(
        mandate.contains(
            "Used on the project&#39;s own decision. Permission requested on October 5, 2026."
        ),
        "{mandate}"
    );
    assert!(!mandate.contains("with permission") && !mandate.contains("icensed"));
    assert!(
        line("Permission Fixture Zendo")
            .contains("Used with permission, recorded on October 6, 2026.")
    );
    assert!(
        line("Declined Fixture Archive").contains("Removed at the rights holder&#39;s request.")
    );
}

#[tokio::test]
async fn a_report_link_prefills_the_form_and_the_form_stores_a_request() {
    let fx = start().await;
    let excerpt = "6f1c2c62-6a43-4a35-9a39-1a1f6a4c2e10";
    let page = send(&fx.app, get(&format!("/rights?excerpt={excerpt}"), None)).await;
    assert!(page.body.contains(&format!("value=\"Excerpt {excerpt}\"")));
    assert!(
        page.body
            .contains(&format!("name=\"excerpt\" value=\"{excerpt}\""))
    );

    let r = send(
        &fx.app,
        post(
            "/rights/request",
            None,
            &format!(
                "name=A+Holder&email=holder%40example.org&target={}&message=Mine&website=&excerpt={excerpt}",
                encode(&format!("Excerpt {excerpt}"))
            ),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.headers[header::LOCATION], "/rights?sent=1#sent");
    let (target, name, email): (String, String, String) =
        sqlx::query_as("select target, requester_name, requester_email from takedown_request")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(target, format!("Excerpt {excerpt}"));
    assert_eq!(name, "A Holder");
    assert_eq!(email, "holder@example.org");

    let thanks = send(&fx.app, get("/rights?sent=1", None)).await;
    assert!(thanks.body.contains("role=\"status\""));
}

#[tokio::test]
async fn the_rights_form_rejects_a_honeypot_missing_fields_and_a_sixth_request_an_hour() {
    let fx = start_with(Site {
        trust_forwarded_for: true,
        ..Site::with_fakes("fake-embed")
    })
    .await;
    let bot = send(
        &fx.app,
        post(
            "/rights/request",
            None,
            "target=x&website=http%3A%2F%2Fspam",
        ),
    )
    .await;
    assert_eq!(
        bot.status,
        StatusCode::SEE_OTHER,
        "the bot sees the thank-you"
    );
    assert_eq!(count(&fx.pool, "takedown_request").await, 0);

    let empty = send(
        &fx.app,
        post(
            "/rights/request",
            None,
            "name=A+Holder&email=a%40b.org&target=+&website=",
        ),
    )
    .await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);
    assert!(empty.body.contains("aria-invalid=\"true\""));
    assert!(empty.body.contains("id=\"r-target-error\""));
    assert!(empty.body.contains("r-target-help r-target-error"));
    assert!(empty.body.contains("Please fix the field marked below."));
    assert!(!empty.body.contains("id=\"r-name-error\""));

    let anonymous = send(
        &fx.app,
        post(
            "/rights/request",
            None,
            "name=+&email=nobody&target=x&website=",
        ),
    )
    .await;
    assert_eq!(anonymous.status, StatusCode::BAD_REQUEST);
    assert!(anonymous.body.contains("id=\"r-name-error\""));
    assert!(anonymous.body.contains("r-email-help r-email-error"));
    assert!(anonymous.body.contains("name@example.org"));
    assert!(
        anonymous
            .body
            .contains("Please fix the fields marked below.")
    );
    assert!(
        anonymous.body.contains("value=\"x\""),
        "the form keeps what was typed"
    );
    assert_eq!(count(&fx.pool, "takedown_request").await, 0);

    for i in 0..2 {
        let r = send(
            &fx.app,
            post(
                "/rights/request",
                None,
                &format!("name=N&email=n%40b.org&target=t{i}"),
            ),
        )
        .await;
        assert_eq!(r.status, StatusCode::SEE_OTHER);
    }
    let sixth = send(
        &fx.app,
        post("/rights/request", None, "name=N&email=n%40b.org&target=t6"),
    )
    .await;
    assert_eq!(sixth.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(sixth.body.contains("Please try again in"));
    assert!(sixth.headers.contains_key(header::RETRY_AFTER));
    assert_eq!(count(&fx.pool, "takedown_request").await, 2);
}

/// The token in a confirmation email's link.
fn token_in(text: &str) -> String {
    let start = text.find("token=").unwrap() + "token=".len();
    text[start..].split_whitespace().next().unwrap().to_owned()
}

#[tokio::test]
async fn a_removal_request_waits_for_its_emailed_link_and_the_link_works_once() {
    let mailer = Arc::new(FakeMailer::default());
    let fx = start_with(Site {
        mailer: mailer.clone(),
        ..Site::with_fakes("fake-embed")
    })
    .await;
    let r = send(
        &fx.app,
        post(
            "/rights/request",
            None,
            "name=A+Holder&email=Holder%40Example.org&target=My+talk&website=",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let thanks = send(&fx.app, get("/rights?sent=1", None)).await;
    assert!(thanks.body.contains("open the link to confirm the request"));

    let sent = mailer.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, "Holder@Example.org");
    assert!(
        sent[0]
            .text
            .contains("https://wt.test/rights/confirm?token=")
    );
    assert!(
        !sent[0].text.contains("My talk"),
        "the email repeats nothing typed"
    );
    let token = token_in(&sent[0].text);
    let status = || async {
        sqlx::query_scalar::<_, String>("select status from takedown_request")
            .fetch_one(&fx.pool)
            .await
            .unwrap()
    };
    assert_eq!(status().await, "unverified");

    let page = send(
        &fx.app,
        get(&format!("/rights/confirm?token={token}"), None),
    )
    .await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Confirm request"));
    assert!(page.body.contains(&format!("value=\"{token}\"")));
    assert_eq!(
        status().await,
        "unverified",
        "following the link changes nothing"
    );

    let done = send(
        &fx.app,
        post("/rights/confirm", None, &format!("token={token}")),
    )
    .await;
    assert_eq!(done.status, StatusCode::OK);
    assert!(done.body.contains("Request confirmed"));
    assert_eq!(status().await, "open");

    let again = send(
        &fx.app,
        post("/rights/confirm", None, &format!("token={token}")),
    )
    .await;
    assert_eq!(again.status, StatusCode::BAD_REQUEST);
    assert!(again.body.contains("This link no longer works"));

    let broken = send(&fx.app, get("/rights/confirm?token=abc", None)).await;
    assert_eq!(broken.status, StatusCode::BAD_REQUEST);
    assert!(broken.body.contains("This link is incomplete"));
}

#[tokio::test]
async fn without_a_working_mailer_the_form_points_to_the_mailbox() {
    let fx = start_with(Site {
        mailer: Arc::new(UnavailableMailer),
        ..Site::with_fakes("fake-embed")
    })
    .await;
    let form = "name=N&email=n%40b.org&target=t&website=";
    let r = send(&fx.app, post("/rights/request", None, form)).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(r.body.contains("rights@wheelturning.org"));
    assert_eq!(count(&fx.pool, "takedown_request").await, 0);

    let fx = start_with(Site {
        mailer: Arc::new(FakeMailer::failing()),
        ..Site::with_fakes("fake-embed")
    })
    .await;
    let r = send(&fx.app, post("/rights/request", None, form)).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(r.body.contains("could not send the confirmation email"));
    let status: String = sqlx::query_scalar("select status from takedown_request")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(
        status, "unverified",
        "a request whose email failed is never acted on"
    );
}

#[tokio::test]
async fn one_address_gets_at_most_three_confirmation_emails_an_hour() {
    let mailer = Arc::new(FakeMailer::default());
    let fx = start_with(Site {
        mailer: mailer.clone(),
        trust_forwarded_for: true,
        ..Site::with_fakes("fake-embed")
    })
    .await;
    for (i, email) in ["v%40b.org", "V%40B.org", "v%40b.org", "v%40b.org"]
        .iter()
        .enumerate()
    {
        let r = send(
            &fx.app,
            Request::post("/rights/request")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header("x-forwarded-for", format!("198.51.100.{i}"))
                .body(Body::from(format!("name=N&email={email}&target=t")))
                .unwrap(),
        )
        .await;
        let expected = if i < 3 {
            StatusCode::SEE_OTHER
        } else {
            StatusCode::TOO_MANY_REQUESTS
        };
        assert_eq!(r.status, expected, "request {i}");
    }
    assert_eq!(mailer.sent().len(), 3);
}

#[tokio::test]
async fn the_accessibility_statement_lists_exactly_the_register_and_stores_a_report() {
    let fx = start().await;
    let r = send(&fx.app, get("/accessibility?page=/about", None)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.body
            .contains("WCAG 3.0 Working Draft of September 10, 2026")
    );
    let register = &*wt_api::accessibility::BUILT;
    for status in wt_api::accessibility::STATUSES {
        let expected = register
            .rows
            .iter()
            .filter(|row| row.status == status)
            .count();
        let marker = format!("data-status=\"{status}\"");
        let listed = r.body.find(&marker).map_or(0, |at| {
            let list = &r.body[at..at + r.body[at..].find("</ul>").unwrap()];
            list.matches("<li>").count()
        });
        assert_eq!(listed, expected, "{status}");
    }
    assert!(
        r.body
            .contains("name=\"page\" type=\"text\" maxlength=\"500\" value=\"/about\"")
    );

    let short = send(
        &fx.app,
        post("/accessibility/report", None, "description=bad"),
    )
    .await;
    assert_eq!(short.status, StatusCode::BAD_REQUEST);
    assert!(short.body.contains("aria-invalid=\"true\""));

    let ok = send(
        &fx.app,
        post(
            "/accessibility/report",
            None,
            "page=%2Fabout&description=The+menu+traps+focus&assistive_tech=NVDA&contact=",
        ),
    )
    .await;
    assert_eq!(ok.status, StatusCode::SEE_OTHER);
    let (page, at, contact): (Option<String>, Option<String>, Option<String>) =
        sqlx::query_as("select page_path, assistive_tech, contact from accessibility_report")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(page.as_deref(), Some("/about"));
    assert_eq!(at.as_deref(), Some("NVDA"));
    assert_eq!(contact, None);
}

#[tokio::test]
async fn icons_and_the_menu_script_need_no_session() {
    let fx = start().await;
    let ico = send(&fx.app, get("/favicon.ico", None)).await;
    assert!(ico.status.is_success(), "{}", ico.status);
    let svg = send(&fx.app, get("/static/brand/favicon.svg", None)).await;
    assert_eq!(svg.status, StatusCode::OK);
    assert_eq!(svg.headers[header::CONTENT_TYPE], "image/svg+xml");
    assert!(!svg.body.contains("<metadata"));
    let js = send(&fx.app, get("/static/menu.js", None)).await;
    assert_eq!(js.status, StatusCode::OK);
    assert!(js.body.contains("Escape"));
    let robots = send(&fx.app, get("/robots.txt", None)).await;
    assert!(robots.body.contains("Disallow: /"));
}

/// Spec 06, Layout and Help and feedback: one `h1`, a unique title, the four landmarks,
/// `lang="en"`, the skip link first, the inline mark hidden, and the footer links in order.
fn check_structure(path: &str, body: &str) -> String {
    assert!(body.contains("<html lang=\"en\">"), "{path}");
    // The progress section's copy sits in an inert <template> and is never rendered with the page.
    let shown = match (body.find("<template"), body.find("</template>")) {
        (Some(a), Some(b)) => format!("{}{}", &body[..a], &body[b..]),
        _ => body.to_owned(),
    };
    assert_eq!(shown.matches("<h1").count(), 1, "{path}: one h1");
    for landmark in [
        "<header class=\"site\">",
        "<nav aria-label=\"Main\"",
        "<main id=\"main\"",
        "<footer class=\"site\">",
    ] {
        assert!(body.contains(landmark), "{path}: {landmark}");
    }
    let first_focusable = ["<a ", "<button", "<input", "<textarea", "<summary"]
        .iter()
        .filter_map(|tag| body.find(tag))
        .min()
        .unwrap();
    assert!(
        body[first_focusable..]
            .starts_with("<a class=\"skip\" href=\"#main\">Skip to main content</a>"),
        "{path}: the skip link is first"
    );
    assert!(body.contains("aria-label=\"Wheel Turning, home\""));
    assert!(body.contains(
        "<svg class=\"mark\" width=\"40\" height=\"40\" aria-hidden=\"true\" focusable=\"false\">"
    ));
    let footer = &body[body.find("<footer").unwrap()..];
    let about = footer.find("href=\"/about\"").unwrap();
    let a11y = footer.find("href=\"/accessibility").unwrap();
    let rights = footer.find("href=\"/rights\"").unwrap();
    assert!(about < a11y && a11y < rights, "{path}: footer order");
    assert!(footer.contains("Excerpts remain the work of their teachers and publishers."));
    assert!(!footer.contains('©') && !footer.to_lowercase().contains("copyright"));
    assert!(
        body.contains("<details class=\"menu narrow\">")
            && body.contains("<summary>Menu</summary>")
    );
    assert!(
        body.contains(
            "<link rel=\"icon\" href=\"/static/brand/favicon.svg\" type=\"image/svg+xml\">"
        )
    );
    let title = &body[body.find("<title>").unwrap() + 7..body.find("</title>").unwrap()];
    title.to_owned()
}

#[tokio::test]
async fn every_page_shares_the_structure_and_has_a_unique_title() {
    let fx = start().await;
    let cookie = join(&fx, "owner").await;
    let mut titles = std::collections::BTreeSet::new();
    for (path, with) in [
        ("/", true),
        ("/about", true),
        ("/rights", false),
        ("/accessibility", false),
        ("/no-such-page", false),
    ] {
        let r = send(&fx.app, get(path, with.then_some(cookie.as_str()))).await;
        let title = check_structure(path, &r.body);
        assert!(
            titles.insert(title.clone()),
            "{path}: title {title} repeats"
        );
    }
    let about = send(&fx.app, get("/about", Some(&cookie))).await;
    assert!(
        about
            .body
            .contains("<dfn>Dana</dfn> is the Sanskrit and Pali word for generosity.")
    );
    assert!(about.body.contains("class=\"summary\""));
    assert!(about.body.contains("aria-current=\"page\">About</a>"));
    assert!(
        about
            .body
            .contains("Wheel Turning is run by an individual as a free project")
    );
    assert!(
        about.body.contains("id=\"hq\""),
        "the header question field off home"
    );
    let home = send(&fx.app, get("/", Some(&cookie))).await;
    assert!(!home.body.contains("id=\"hq\""), "but not on home");
    let rights = send(&fx.app, get("/rights", None)).await;
    assert!(
        !rights.body.contains("name=\"csrf\""),
        "no session, no question field"
    );
}

#[tokio::test]
async fn asks_past_the_hourly_limit_get_the_friendly_429() {
    let mut site = Site::with_fakes("fake-embed");
    site.limits = Limits::new(2);
    let fx = start_with(site).await;
    let cookie = join(&fx, "owner").await;
    let token = csrf(&send(&fx.app, get("/", Some(&cookie))).await.body);
    let ask = || {
        post(
            "/ask",
            Some(&cookie),
            &format!("q=anger&csrf={}", encode(&token)),
        )
    };
    for _ in 0..2 {
        assert_eq!(send(&fx.app, ask()).await.status, StatusCode::SEE_OTHER);
    }
    let r = send(&fx.app, ask()).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        r.body.contains(
            "You have asked a lot of questions this hour. Please try again in 30 minutes."
        )
    );
    assert_eq!(r.headers[header::RETRY_AFTER], "1800");
    let other = join(&fx, "someone else").await;
    let theirs = csrf(&send(&fx.app, get("/", Some(&other))).await.body);
    let r = send(
        &fx.app,
        post(
            "/ask",
            Some(&other),
            &format!("q=anger&csrf={}", encode(&theirs)),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "the limit is per tester");
}
