//! URL ingestion (Spec 08, Ingesting from URLs) against a local test server: the evidence gate,
//! raw copies and sidecars, `robots.txt`, private addresses and redirects, media refusal,
//! format sniffing, HTML and DOCX conversion, drafts, source anchors, politeness, and the
//! permission record. Nothing leaves the machine.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use sqlx::PgPool;
use sqlx::types::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use wt_core::objects::{LocalObjects, ObjectStore};
use wt_core::records::Records as _;
use wt_db::test_support::TestDb;
use wt_ingest::config::{LicenseSettings, UrlPolicy};
use wt_ingest::http::Exclusions;
use wt_ingest::texts::convert::Kind;
use wt_ingest::texts::manifest::Format;
use wt_ingest::texts::url::{FetchOptions, UrlReport, fetch_one};
use wt_ingest::texts::web::{Validators, WebFetcher, WebSettings};
use wt_ml::FakeEmbedder;

const PAGE: &str = r#"<!doctype html><html><head><title>Fixture Site</title>
<meta property="og:title" content="The Fixture Discourse">
<meta name="author" content="Fixture Author">
<meta name="citation_publication_date" content="1901">
</head><body><nav><a href="/">Home</a> <a href="/about">About</a></nav>
<article><h1>The Fixture Discourse</h1>
<p>The first teaching says the mind is like a clear pond on a still morning, and that is all there is to say about it.</p>
<p>The second teaching says the wind moves the water and the moon breaks into many pieces for a while.</p>
<p>The third teaching says the water settles again and the moon is whole as it was before the wind.</p>
</article><footer>Site footer with links.</footer></body></html>"#;

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
    let dir = std::env::temp_dir().join(format!("wt-url-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("User-agent: *\nDisallow: /private\n"),
        )
        .mount(&server)
        .await;
    let objects = LocalObjects::new(dir.join("objects"));
    Fx {
        _db: db,
        pool,
        dir,
        objects,
        server,
    }
}

impl Fx {
    fn url(&self, p: &str) -> String {
        format!("{}{p}", self.server.uri())
    }

    /// Approved evidence covering `/docs/` on the test server.
    async fn evidence(&self) -> Uuid {
        sqlx::query_scalar(
            "insert into license_evidence (publisher, license_code, license_url, evidence_url,
                                           evidence_snapshot_ref, snapshot_sha256, captured_at,
                                           reviewed_by, reviewed_at, allowed, url_prefixes)
             values ('Fixture Press', 'public-domain', 'https://example.invalid/l',
                     'https://example.invalid/e', 'records/x', 'x', now(), 'reviewer', now(),
                     true, array[$1])
             returning id",
        )
        .bind(self.url("/docs/"))
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    fn fetcher(&self) -> WebFetcher {
        WebFetcher::new(
            WebSettings {
                fetch_delay: Duration::from_millis(150),
                allow_addresses: vec!["127.0.0.1".parse().unwrap()],
                ..WebSettings::default()
            },
            Exclusions::new(Vec::new()),
        )
    }

    async fn serve(&self, p: &str, body: impl Into<Vec<u8>>, content_type: &str) {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body.into(), content_type)
                    .insert_header("etag", "\"v1\""),
            )
            .mount(&self.server)
            .await;
    }

    async fn fetch(&self, p: &str, yes: bool) -> UrlReport {
        let mut conn = self.pool.acquire().await.unwrap();
        let codes = LicenseSettings::default().allowed_license_codes;
        let inbox = self.dir.join("inbox");
        fetch_one(
            &mut conn,
            &self.objects,
            &FakeEmbedder::new("fake"),
            &self.fetcher(),
            &self.url(p),
            &FetchOptions {
                allowed_codes: &codes,
                actor: "owner",
                yes,
                inbox: &inbox,
                pdf_env: None,
            },
        )
        .await
        .unwrap()
    }

    async fn received(&self, verb: &str, p: &str) -> usize {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.method.as_str() == verb && r.url.path() == p)
            .count()
    }
}

#[tokio::test]
async fn an_uncovered_url_gets_a_head_request_only_and_a_covered_one_is_stored_and_drafted() {
    let fx = start().await;
    fx.evidence().await;
    Mock::given(method("HEAD"))
        .and(path("/elsewhere/page.html"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/html"))
        .mount(&fx.server)
        .await;
    fx.serve("/elsewhere/page.html", PAGE, "text/html").await;
    let outside = fx.fetch("/elsewhere/page.html", false).await;
    assert!(outside.stopped.unwrap().contains("only a HEAD request"));
    assert_eq!(
        outside.head.unwrap().content_type.as_deref(),
        Some("text/html")
    );
    assert_eq!(
        fx.received("GET", "/elsewhere/page.html").await,
        0,
        "no body was downloaded"
    );
    assert!(outside.raw_key.is_none());

    fx.serve("/docs/discourse.html", PAGE, "text/html; charset=utf-8")
        .await;
    let inside = fx.fetch("/docs/discourse.html", false).await;
    let raw_key = inside.raw_key.clone().unwrap();
    assert!(raw_key.starts_with("texts-source/127.0.0.1/"));
    assert_eq!(fx.objects.get(&raw_key).unwrap().unwrap(), PAGE.as_bytes());
    let sidecar: serde_json::Value =
        serde_json::from_slice(&fx.objects.get(&format!("{raw_key}.json")).unwrap().unwrap())
            .unwrap();
    assert_eq!(sidecar["etag"], "\"v1\"");
    assert_eq!(inside.kind, Some(Kind::Readable(Format::Html)));
    let draft = std::fs::read_to_string(inside.draft.as_ref().unwrap()).unwrap();
    assert!(draft.contains("title = \"The Fixture Discourse\""));
    assert!(draft.contains("author = \"Fixture Author\""));
    assert!(
        draft.contains("# translator"),
        "the translator is asked, never guessed"
    );
    let dry = inside.import.unwrap();
    assert_eq!(dry.units, 3);
    assert!(
        !dry.first
            .iter()
            .any(|u| u.start.contains("Home") || u.start.contains("footer"))
    );
    assert!(!dry.applied);

    // --yes imports a born-digital HTML page with a clear title, with text-fragment anchors.
    let applied = fx.fetch("/docs/discourse.html", true).await;
    assert!(applied.import.unwrap().applied);
    let anchor: String =
        sqlx::query_scalar("select source_anchor from text_unit order by ordinal limit 1")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert!(
        anchor.starts_with("#:~:text=The%20first%20teaching"),
        "{anchor}"
    );
}

#[tokio::test]
async fn robots_private_addresses_redirects_and_media_are_refused() {
    let fx = start().await;
    fx.evidence().await;
    let fetcher = fx.fetcher();
    let get = |p: &str| url::Url::parse(&fx.url(p)).unwrap();

    // robots.txt disallows, and nothing overrides it.
    fx.serve("/private/doc.html", PAGE, "text/html").await;
    let err = fetcher
        .get(&get("/private/doc.html"), &Validators::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("robots.txt"), "{err}");

    // A redirect into a link-local address is checked again and blocked.
    Mock::given(method("GET"))
        .and(path("/docs/moved.html"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "http://169.254.169.254/latest"),
        )
        .mount(&fx.server)
        .await;
    let err = fetcher
        .get(&get("/docs/moved.html"), &Validators::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a public address"), "{err}");

    // Loopback itself, when not allowed for a test.
    let strict = WebFetcher::new(WebSettings::default(), Exclusions::new(Vec::new()));
    let err = strict
        .get(&get("/docs/discourse.html"), &Validators::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a public address"), "{err}");

    // Audio and video are refused.
    fx.serve("/docs/talk.mp3", vec![0u8; 64], "audio/mpeg")
        .await;
    let err = fetcher
        .get(&get("/docs/talk.mp3"), &Validators::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("audio or video"), "{err}");

    // A conditional refetch of an unchanged document is a no-op.
    Mock::given(method("GET"))
        .and(path("/docs/same.html"))
        .and(wiremock::matchers::header("if-none-match", "\"v1\""))
        .respond_with(ResponseTemplate::new(304))
        .mount(&fx.server)
        .await;
    let got = fetcher
        .get(
            &get("/docs/same.html"),
            &Validators {
                etag: Some("\"v1\"".into()),
                last_modified: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(got, wt_ingest::texts::web::Got::NotModified);
}

fn docx() -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut buf);
    let opts = zip::write::SimpleFileOptions::default();
    zip.start_file("[Content_Types].xml", opts).unwrap();
    zip.write_all(b"<Types/>").unwrap();
    zip.start_file("docProps/core.xml", opts).unwrap();
    zip.write_all(b"<cp:coreProperties><dc:title>The Fixture Letter</dc:title><dc:creator>Fixture Writer</dc:creator></cp:coreProperties>").unwrap();
    zip.start_file("word/document.xml", opts).unwrap();
    zip.write_all(
        br#"<w:document><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>On Sitting</w:t></w:r></w:p>
<w:p><w:r><w:t xml:space="preserve">The student sat in the hall before dawn, and </w:t></w:r><w:r><w:t>the bell rang three times.</w:t></w:r></w:p>
<w:p><w:r><w:t>Nothing was asked of her except to breathe and to notice the breath.</w:t></w:r></w:p>
</w:body></w:document>"#,
    )
    .unwrap();
    zip.finish().unwrap();
    buf.into_inner()
}

#[tokio::test]
async fn formats_are_sniffed_by_signature_and_docx_keeps_headings_and_paragraphs() {
    let fx = start().await;
    fx.evidence().await;
    fx.serve(
        "/docs/actually-a-pdf.html",
        b"%PDF-1.4 fixture".to_vec(),
        "text/html",
    )
    .await;
    let pdf = fx.fetch("/docs/actually-a-pdf.html", false).await;
    assert_eq!(pdf.kind, Some(Kind::Readable(Format::Pdf)));
    assert!(
        pdf.stopped.unwrap().contains("poppler"),
        "a PDF needs the PDF tools"
    );

    fx.serve("/docs/letter.docx", docx(), "application/octet-stream")
        .await;
    let letter = fx.fetch("/docs/letter.docx", true).await;
    assert_eq!(letter.kind, Some(Kind::Readable(Format::Docx)));
    assert_eq!(
        letter.meta.as_ref().unwrap().title.as_deref(),
        Some("The Fixture Letter")
    );
    let report = letter.import.unwrap();
    assert!(!report.applied, "--yes imports only born-digital HTML");
    assert_eq!(report.sections_kept, vec!["On Sitting"]);
    assert_eq!(report.units, 2);
    assert!(
        report.first[0]
            .start
            .starts_with("The student sat in the hall before dawn, and the bell")
    );
}

#[tokio::test]
async fn the_fetcher_names_itself_waits_between_requests_and_uses_one_connection_per_host() {
    let fx = start().await;
    for i in 0..3 {
        fx.serve(
            &format!("/docs/{i}.txt"),
            "plain fixture words",
            "text/plain",
        )
        .await;
    }
    let fetcher = fx.fetcher();
    let started = Instant::now();
    let urls: Vec<url::Url> = (0..3)
        .map(|i| url::Url::parse(&fx.url(&format!("/docs/{i}.txt"))).unwrap())
        .collect();
    let v = Validators::default();
    let (a, b, c) = tokio::join!(
        fetcher.get(&urls[0], &v),
        fetcher.get(&urls[1], &v),
        fetcher.get(&urls[2], &v)
    );
    a.unwrap();
    b.unwrap();
    c.unwrap();
    // robots.txt and three documents, one at a time, at least 150 ms apart.
    assert!(
        started.elapsed() >= Duration::from_millis(450),
        "{:?}",
        started.elapsed()
    );
    let requests = fx.server.received_requests().await.unwrap();
    assert!(requests.iter().all(|r| {
        r.headers
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ua| ua.starts_with("WheelTurning/"))
    }));
    assert_eq!(
        fx.received("GET", "/robots.txt").await,
        1,
        "robots.txt is read once per host"
    );
}

#[tokio::test]
async fn a_permission_needs_a_grantor_and_prefixes_and_a_person_s_approval() {
    let fx = start().await;
    let records = wt_core::records::LocalRecords::new(fx.dir.join("records"));
    let statement = records
        .store(b"Yes, you may excerpt our translations with attribution.")
        .unwrap();
    let mut conn = fx.pool.acquire().await.unwrap();
    let none: Vec<String> = Vec::new();
    let refused = wt_db::license::record_permission(
        &mut conn,
        &wt_db::license::Permission {
            grantor: "Fixture Press",
            prefixes: &none,
            covers: "excerpting with attribution",
            statement: &statement,
            given_on: "2026-10-01",
            recorded_by: "owner",
        },
    )
    .await;
    assert!(refused.is_err(), "the permission_has_scope constraint");

    let prefixes = vec![fx.url("/docs/")];
    let id = wt_db::license::record_permission(
        &mut conn,
        &wt_db::license::Permission {
            grantor: "Fixture Press",
            prefixes: &prefixes,
            covers: "excerpting with attribution",
            statement: &statement,
            given_on: "2026-10-01",
            recorded_by: "owner",
        },
    )
    .await
    .unwrap();
    // Unapproved, it covers nothing.
    fx.serve("/docs/granted.html", PAGE, "text/html").await;
    Mock::given(method("HEAD"))
        .and(path("/docs/granted.html"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&fx.server)
        .await;
    assert!(fx.fetch("/docs/granted.html", false).await.head.is_some());
    let codes = LicenseSettings::default().allowed_license_codes;
    assert_eq!(
        wt_db::license::approve(&mut conn, id, "Reviewer", &codes)
            .await
            .unwrap(),
        wt_db::license::Approval::Approved
    );
    let covered = fx.fetch("/docs/granted.html", false).await;
    let by = covered.covered_by.unwrap();
    assert_eq!(
        (by.kind.as_str(), by.grantor.as_deref()),
        ("permission", Some("Fixture Press"))
    );
    let draft = std::fs::read_to_string(covered.draft.unwrap()).unwrap();
    assert!(draft.contains("rights_holder = \"Fixture Press\""));
}

#[test]
fn an_aggregator_host_cannot_be_covered_as_a_whole() {
    let policy = UrlPolicy::default();
    assert!(policy.check_prefix("https://terebess.hu/").is_err());
    assert!(policy.check_prefix("https://www.terebess.hu").is_err());
    assert!(
        policy
            .check_prefix("https://terebess.hu/zen/fixture.html")
            .is_ok()
    );
    assert!(policy.check_prefix("https://www.bdkamerica.org/").is_ok());
}
