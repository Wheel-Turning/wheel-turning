//! The crawl, refresh and `wt intake` (Spec 08, Ingesting from URLs and Guided loading), against
//! a local test server and with scripted answers.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use sqlx::PgPool;
use sqlx::types::Uuid;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use wt_core::objects::LocalObjects;
use wt_db::test_support::TestDb;
use wt_ingest::config::LicenseSettings;
use wt_ingest::http::Exclusions;
use wt_ingest::texts::crawl::{self, Crawl};
use wt_ingest::texts::intake::{self, IntakeEnv, Outcome, Prompt};
use wt_ingest::texts::url::{FetchOptions, fetch_one, urls_file};
use wt_ingest::texts::web::{WebFetcher, WebSettings};
use wt_ingest::texts::{TextError, refresh};
use wt_ml::FakeEmbedder;

fn page(paragraphs: &[&str]) -> String {
    let body: String = paragraphs.iter().map(|p| format!("<p>{p}</p>")).collect();
    format!(
        "<!doctype html><html><head><title>The Fixture Discourse</title></head><body>\
         <article><h1>The Fixture Discourse</h1>{body}</article></body></html>"
    )
}

const FIRST: &str =
    "The first teaching says the mind is like a clear pond on a still morning, and that is all.";
const SECOND: &str =
    "The second teaching says the wind moves the water and the moon breaks into pieces.";
const THIRD: &str =
    "The third teaching says the water settles again and the moon is whole as before.";

struct Fx {
    _db: TestDb,
    pool: PgPool,
    dir: PathBuf,
    objects: LocalObjects,
    server: MockServer,
}

async fn start() -> Fx {
    let db = TestDb::start().await.expect("docker must be running");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();
    let dir = std::env::temp_dir().join(format!("wt-intake-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let server = MockServer::start().await;
    robots(&server).await;
    let objects = LocalObjects::new(dir.join("objects"));
    Fx {
        _db: db,
        pool,
        dir,
        objects,
        server,
    }
}

async fn robots(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(server)
        .await;
}

impl Fx {
    fn url(&self, p: &str) -> String {
        format!("{}{p}", self.server.uri())
    }

    fn fetcher(&self) -> WebFetcher {
        WebFetcher::new(
            WebSettings {
                fetch_delay: Duration::from_millis(10),
                allow_addresses: vec!["127.0.0.1".parse().unwrap()],
                ..WebSettings::default()
            },
            Exclusions::new(Vec::new()),
        )
    }

    async fn evidence_for(&self, prefix: &str) -> Uuid {
        sqlx::query_scalar(
            "insert into license_evidence (publisher, license_code, license_url, evidence_url,
                                           evidence_snapshot_ref, snapshot_sha256, captured_at,
                                           reviewed_by, reviewed_at, allowed, url_prefixes)
             values ('Fixture Press', 'public-domain', 'https://example.invalid/l',
                     'https://example.invalid/e', 'records/x', 'x', now(), 'reviewer', now(),
                     true, array[$1])
             returning id",
        )
        .bind(self.url(prefix))
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn serve(&self, p: &str, body: String) {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body.into_bytes(), "text/html")
                    .insert_header("etag", "\"v1\""),
            )
            .mount(&self.server)
            .await;
    }

    async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn a_crawl_lists_matching_links_in_scope_and_imports_nothing() {
    let fx = start().await;
    fx.evidence_for("/docs/").await;
    let index = format!(
        r##"<html><body><a href="/docs/a.html">A</a> <a href="/docs/b.html">B</a>
        <a href="/docs/c.html#part">C</a> <a href="/docs/notes.txt">notes</a>
        <a href="{}">elsewhere</a> <a href="#top">top</a></body></html>"##,
        fx.url("/other/x.html")
    );
    fx.serve("/docs/index.html", index).await;
    let mut conn = fx.pool.acquire().await.unwrap();
    let follow = crawl::pattern(r"\.html$").unwrap();
    let inbox = fx.dir.join("inbox");
    let report = crawl::crawl(
        &mut conn,
        &fx.fetcher(),
        &Crawl {
            start: &fx.url("/docs/index.html"),
            depth: 1,
            follow: Some(&follow),
            max_pages: 2,
            inbox: &inbox,
        },
    )
    .await
    .unwrap();
    assert_eq!(report.pages_read, 1);
    assert_eq!(
        report.candidates,
        vec![fx.url("/docs/a.html"), fx.url("/docs/b.html")],
        "--max-pages"
    );
    assert_eq!(report.outside_scope, vec![fx.url("/other/x.html")]);
    let list = std::fs::read_to_string(report.list.unwrap()).unwrap();
    assert!(
        urls_file(&list).is_empty(),
        "nothing is selected until a person uncomments it"
    );
    assert!(list.contains(&format!("# {}", fx.url("/docs/a.html"))));
    assert_eq!(fx.count("select count(*) from text_edition").await, 0);
}

#[tokio::test]
async fn refresh_is_a_no_op_when_unchanged_and_diffs_a_change_before_applying() {
    let fx = start().await;
    fx.evidence_for("/docs/").await;
    fx.serve("/docs/discourse.html", page(&[FIRST, SECOND, THIRD]))
        .await;
    let mut conn = fx.pool.acquire().await.unwrap();
    let codes = LicenseSettings::default().allowed_license_codes;
    let inbox = fx.dir.join("inbox");
    let fetched = fetch_one(
        &mut conn,
        &fx.objects,
        &FakeEmbedder::new("fake"),
        &fx.fetcher(),
        &fx.url("/docs/discourse.html"),
        &FetchOptions {
            allowed_codes: &codes,
            actor: "owner",
            yes: true,
            inbox: &inbox,
            pdf_env: None,
        },
    )
    .await
    .unwrap();
    assert!(fetched.import.unwrap().applied);
    let edition: Uuid = sqlx::query_scalar("select id from text_edition")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    let run = |apply: bool| {
        let pool = fx.pool.clone();
        let objects = LocalObjects::new(fx.dir.join("objects"));
        let fetcher = fx.fetcher();
        let codes = codes.clone();
        async move {
            let mut conn = pool.acquire().await.unwrap();
            refresh::refresh(
                &mut conn,
                &objects,
                &FakeEmbedder::new("fake"),
                &fetcher,
                None,
                &codes,
                edition,
                apply,
                "owner",
            )
            .await
            .unwrap()
        }
    };

    // Unchanged: the server answers 304 to the stored ETag.
    fx.server.reset().await;
    robots(&fx.server).await;
    Mock::given(method("GET"))
        .and(path("/docs/discourse.html"))
        .and(header("if-none-match", "\"v1\""))
        .respond_with(ResponseTemplate::new(304))
        .mount(&fx.server)
        .await;
    let same = run(false).await;
    assert!(!same.changed && same.import.is_none());

    // Changed: the second paragraph is reworded and a fourth added.
    fx.server.reset().await;
    robots(&fx.server).await;
    let reworded = "The second teaching says a gust moves the water and the moon breaks apart.";
    let fourth = "The fourth teaching says that the pond, the wind and the moon were never apart.";
    fx.serve(
        "/docs/discourse.html",
        page(&[FIRST, reworded, THIRD, fourth]),
    )
    .await;
    let before: Vec<String> = sqlx::query_scalar("select text from text_unit order by ordinal")
        .fetch_all(&fx.pool)
        .await
        .unwrap();
    let diff = run(false).await;
    assert!(diff.changed);
    assert_eq!(diff.changed_units.len(), 1, "{diff:?}");
    assert_eq!(diff.added.len(), 1);
    assert!(diff.removed.is_empty());
    let unchanged: Vec<String> = sqlx::query_scalar("select text from text_unit order by ordinal")
        .fetch_all(&fx.pool)
        .await
        .unwrap();
    assert_eq!(unchanged, before, "serving continues until --apply");

    let applied = run(true).await;
    assert!(applied.import.unwrap().applied);
    assert_eq!(fx.count("select count(*) from text_unit").await, 4);
    assert_eq!(
        fx.count(
            "select count(*) from text_edition where state = 'imported' and fetched_at is not null"
        )
        .await,
        1,
        "the new version waits for its proofread"
    );
}

/// Answers from a script; running out fails the test.
struct Script(VecDeque<&'static str>);

impl Prompt for Script {
    fn say(&mut self, text: &str) {
        eprintln!("{text}");
    }

    fn ask(&mut self, question: &str, default: &str) -> Result<String, TextError> {
        let answer = self
            .0
            .pop_front()
            .unwrap_or_else(|| panic!("no scripted answer for {question:?}"));
        Ok(if answer.is_empty() {
            default.to_owned()
        } else {
            answer.to_owned()
        })
    }
}

#[tokio::test]
async fn intake_blocks_without_evidence_and_completes_with_a_typed_reviewer() {
    let fx = start().await;
    let inbox = fx.dir.join("inbox");
    let folder = inbox.join("texts/fixture-sutra");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("text.txt"),
        format!("{FIRST}\n\n{SECOND}\n\n{THIRD}\n"),
    )
    .unwrap();
    assert_eq!(intake::pending(&inbox), vec![folder.clone()]);
    let records = wt_core::records::LocalRecords::new(fx.dir.join("records"));
    let codes = LicenseSettings::default().allowed_license_codes;
    let fetcher = fx.fetcher();
    let env = IntakeEnv {
        allowed_codes: &codes,
        records: &records,
        fetcher: &fetcher,
        pdf_env: None,
    };
    let mut conn = fx.pool.acquire().await.unwrap();
    let embedder = FakeEmbedder::new("fake");

    // No evidence: blocked, with what is missing, and nothing applied.
    let mut script = Script(VecDeque::from([
        "",
        "Fixture Sutra",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
    ]));
    let walked = intake::walk(
        &mut conn,
        &fx.objects,
        &embedder,
        &mut script,
        &env,
        &inbox,
        &folder,
    )
    .await
    .unwrap();
    assert!(matches!(walked.outcome, Outcome::Blocked(_)));
    assert!(
        std::fs::read_to_string(folder.join("BLOCKED.txt"))
            .unwrap()
            .contains("no approved license evidence")
    );
    assert_eq!(fx.count("select count(*) from text_edition").await, 0);

    // With the license page, a typed reviewer, the proposed scheme and a yes.
    Mock::given(method("GET"))
        .and(path("/license.html"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            b"<html><body><p>This translation was published in 1881 and is in the public domain.</p></body></html>".to_vec(),
            "text/html",
        ))
        .mount(&fx.server)
        .await;
    let license = fx.url("/license.html");
    let license: &'static str = Box::leak(license.into_boxed_str());
    let mut script = Script(VecDeque::from([
        "",
        "Fixture Sutra",
        "",
        "",
        "Fixture 1881",
        "",
        "",
        "",
        license,
        "",
        "published in 1881",
        "Reviewer Fixture",
        "",
        "yes",
    ]));
    let walked = intake::walk(
        &mut conn,
        &fx.objects,
        &embedder,
        &mut script,
        &env,
        &inbox,
        &folder,
    )
    .await
    .unwrap();
    assert_eq!(walked.outcome, Outcome::Applied);
    assert!(walked.folder.starts_with(inbox.join("_done")));
    assert!(!folder.exists(), "the folder moved to _done");
    assert!(intake::pending(&inbox).is_empty());
    assert_eq!(fx.count("select count(*) from text_unit").await, 3);
    assert_eq!(
        fx.count("select count(*) from license_evidence where allowed and reviewed_by = 'Reviewer Fixture'").await,
        1
    );
}

#[tokio::test]
async fn a_url_under_a_mandated_prefix_needs_no_evidence_of_its_own() {
    let fx = start().await;
    let evidence: Uuid = sqlx::query_scalar(
        "insert into license_evidence (publisher, license_code, evidence_snapshot_ref,
                                       snapshot_sha256, captured_at, reviewed_by, reviewed_at,
                                       allowed, kind, url_prefixes, mandated_by, rationale,
                                       display_notice)
         values ('Fixture Publisher', 'operator-mandate', 'records/m', 'm', now(), 'Owner',
                 now(), true, 'mandate', array[$1], 'Owner', 'to spread the teachings',
                 'Excerpted with attribution from {title}, {publisher}.')
         returning id",
    )
    .bind(fx.url("/"))
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into source_policy (key, publisher, url_prefixes, basis, license_evidence_id)
         values ('fixturepub', 'Fixture Publisher', array[$1], 'mandate', $2)",
    )
    .bind(fx.url("/"))
    .bind(evidence)
    .execute(&fx.pool)
    .await
    .unwrap();
    fx.serve("/sutra.html", page(&[FIRST, SECOND, THIRD])).await;
    let mut conn = fx.pool.acquire().await.unwrap();
    let codes = LicenseSettings::default().allowed_license_codes;
    let inbox = fx.dir.join("inbox");
    let report = fetch_one(
        &mut conn,
        &fx.objects,
        &FakeEmbedder::new("fake"),
        &fx.fetcher(),
        &fx.url("/sutra.html"),
        &FetchOptions {
            allowed_codes: &codes,
            actor: "owner",
            yes: true,
            inbox: &inbox,
            pdf_env: None,
        },
    )
    .await
    .unwrap();
    let by = report.covered_by.unwrap();
    assert_eq!(
        (by.kind.as_str(), by.source_key.as_deref()),
        ("mandate", Some("fixturepub"))
    );
    assert!(report.import.unwrap().applied);
    assert_eq!(
        fx.count("select count(*) from text_edition where source_key = 'fixturepub' and rights_holder = 'Fixture Publisher'").await,
        1
    );
    // Switched off, the source covers nothing.
    sqlx::query("update source_policy set enabled = false")
        .execute(&fx.pool)
        .await
        .unwrap();
    Mock::given(method("HEAD"))
        .and(path("/other.html"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&fx.server)
        .await;
    let off = fetch_one(
        &mut conn,
        &fx.objects,
        &FakeEmbedder::new("fake"),
        &fx.fetcher(),
        &fx.url("/other.html"),
        &FetchOptions {
            allowed_codes: &codes,
            actor: "owner",
            yes: true,
            inbox: &inbox,
            pdf_env: None,
        },
    )
    .await
    .unwrap();
    assert!(off.head.is_some() && off.covered_by.is_none());
}
