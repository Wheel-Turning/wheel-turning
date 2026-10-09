//! Spec 08, Import pipeline: `wt text import` on synthetic fixtures built here, an EPUB and an
//! HTML page, with a fake embedder and a real database. Every word is invented.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::path::{Path, PathBuf};

use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::objects::{LocalObjects, ObjectStore};
use wt_db::test_support::TestDb;
use wt_ingest::config::LicenseSettings;
use wt_ingest::texts::{self, TextError};
use wt_ml::FakeEmbedder;

struct Fx {
    _db: TestDb,
    pool: PgPool,
    dir: PathBuf,
    objects: LocalObjects,
}

async fn start() -> Fx {
    let db = TestDb::start().await.expect("docker must be running");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();
    let dir = std::env::temp_dir().join(format!("wt-texts-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let objects = LocalObjects::new(dir.join("objects"));
    Fx {
        _db: db,
        pool,
        dir,
        objects,
    }
}

async fn evidence(pool: &PgPool, code: &str, allowed: bool) -> Uuid {
    sqlx::query_scalar(
        "insert into license_evidence (publisher, license_code, license_url, evidence_url,
                                       evidence_snapshot_ref, snapshot_sha256, captured_at,
                                       reviewed_by, reviewed_at, allowed)
         values ('Fixture Press', $1, 'https://example.invalid/l', 'https://example.invalid/e',
                 'records/x', 'x', now(), 'reviewer', now(), $2) returning id",
    )
    .bind(code)
    .bind(allowed)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn xhtml(title: &str, paragraphs: &[&str]) -> String {
    let body: String = paragraphs.iter().map(|p| format!("<p>{p}</p>")).collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><html xmlns=\"http://www.w3.org/1999/xhtml\">\
         <head><title>{title}</title></head><body><h1>{title}</h1>{body}</body></html>"
    )
}

/// An EPUB whose spine lists an introduction and two chapters of numbered verses.
fn write_epub(path: &Path) {
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("mimetype", stored).unwrap();
    zip.write_all(b"application/epub+zip").unwrap();
    let files = [
        (
            "META-INF/container.xml",
            r#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#.to_owned(),
        ),
        (
            "OEBPS/content.opf",
            r#"<?xml version="1.0"?><package><manifest>
                 <item id="intro" href="text/intro.xhtml" media-type="application/xhtml+xml"/>
                 <item id="c1" href="text/c1.xhtml" media-type="application/xhtml+xml"/>
                 <item id="c2" href="text/c2.xhtml" media-type="application/xhtml+xml"/>
               </manifest><spine><itemref idref="intro"/><itemref idref="c1"/><itemref idref="c2"/></spine></package>"#.to_owned(),
        ),
        (
            "OEBPS/text/intro.xhtml",
            xhtml("Translator's Introduction", &["This fixture translation was made for tests and carries the translator's own words."]),
        ),
        (
            "OEBPS/text/c1.xhtml",
            xhtml("Chapter 1: The Pairs", &[
                "1. All that we are arises from the mind, the fixture says, and the mind leads.",
                "2. Speak or act with a quiet mind, and ease follows like a shadow that never leaves.",
            ]),
        ),
        (
            "OEBPS/text/c2.xhtml",
            xhtml("Chapter 2: Attention", &[
                "3. Attention is the path to the deathless, and heedlessness the path of loss.",
            ]),
        ),
    ];
    let deflated = zip::write::SimpleFileOptions::default();
    for (name, body) in files {
        zip.start_file(name, deflated).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

fn write_manifest(dir: &Path, evidence: Uuid, extra: &str) -> PathBuf {
    write_epub(&dir.join("fixture.epub"));
    let manifest = format!(
        r#"role = "corpus"

[work]
title = "Fixture Sutra"
author = "Fixture Author"
tradition = "fixture"

[edition]
label = "Fixture 1901"
translator = "Fixture Translator"
source_url = "https://example.invalid/fixture"
rights_holder = "public domain"
license_evidence_id = "{evidence}"

[source]
files = ["fixture.epub"]
format = "epub"
{extra}

[units]
scheme = "numbered"
locator_prefix = "fixture"
number_pattern = '^\s*(\d+)\.\s'
label_template = "{{work}}, verse {{n}}"

[aliases]
"fixture:1" = ["All that we are"]
"#
    );
    let path = dir.join("manifest.toml");
    std::fs::write(&path, manifest).unwrap();
    path
}

async fn count(pool: &PgPool, table: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        // Table names come from the fixed lists in these tests.
        "select count(*) from {table}"
    )))
    .fetch_one(pool)
    .await
    .unwrap()
}

const TABLES: [&str; 6] = [
    "person",
    "work",
    "text_edition",
    "text_unit",
    "work_locator",
    "import_run",
];

async fn counts(pool: &PgPool) -> Vec<i64> {
    let mut out = Vec::new();
    for t in TABLES {
        out.push(count(pool, t).await);
    }
    out
}

fn allowed() -> Vec<String> {
    LicenseSettings::default().allowed_license_codes
}

#[tokio::test]
async fn a_dry_run_reports_and_an_apply_writes_once() {
    let fx = start().await;
    let ev = evidence(&fx.pool, "public-domain", true).await;
    let manifest = write_manifest(&fx.dir, ev, "");
    let embedder = FakeEmbedder::new("fake-bge-m3");
    let mut conn = fx.pool.acquire().await.unwrap();

    let before = counts(&fx.pool).await;
    let dry = texts::import(
        &mut conn,
        &fx.objects,
        &embedder,
        &manifest,
        &allowed(),
        "owner",
        false,
        None,
    )
    .await
    .unwrap();
    assert_eq!(counts(&fx.pool).await, before, "a dry run changes nothing");
    assert!(!dry.applied);
    assert_eq!(dry.sections_excluded, vec!["Translator's Introduction"]);
    assert_eq!(dry.sections_kept.len(), 2);
    assert_eq!(dry.units, 3);
    assert_eq!(dry.first[0].locator, "fixture:1");
    assert_eq!(dry.first[0].label, "Fixture Sutra, verse 1");
    assert!(dry.first[0].start.starts_with("All that we are"));

    let applied = texts::import(
        &mut conn,
        &fx.objects,
        &embedder,
        &manifest,
        &allowed(),
        "owner",
        true,
        None,
    )
    .await
    .unwrap();
    assert!(applied.applied && !applied.unchanged);
    let (state, excerptable): (String, bool) = sqlx::query_as(
        "select state::text, excerptable from text_edition where edition_label = 'Fixture 1901'",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!((state.as_str(), excerptable), ("imported", true));
    assert_eq!(count(&fx.pool, "text_unit").await, 3);
    let aliases: Vec<String> =
        sqlx::query_scalar("select aliases from work_locator where locator = 'fixture:1'")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(aliases, vec!["All that we are"]);
    let author: String = sqlx::query_scalar(
        "select p.display_name from work w join person p on p.id = w.author_person_id
         where w.title = 'Fixture Sutra'",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(author, "Fixture Author");
    assert!(
        fx.objects
            .get("texts-source/fixture-sutra/fixture-1901/fixture.epub")
            .unwrap()
            .is_some()
    );
    // Not servable until a person proofreads it.
    assert_eq!(count(&fx.pool, "servable_text_unit").await, 0);

    let after = counts(&fx.pool).await;
    let again = texts::import(
        &mut conn,
        &fx.objects,
        &embedder,
        &manifest,
        &allowed(),
        "owner",
        true,
        None,
    )
    .await
    .unwrap();
    assert!(again.unchanged && !again.applied);
    assert_eq!(
        counts(&fx.pool).await,
        after,
        "an unchanged import is a no-op"
    );
}

#[tokio::test]
async fn evidence_must_exist_be_approved_and_carry_an_allowed_code() {
    let fx = start().await;
    let embedder = FakeEmbedder::new("fake");
    let mut conn = fx.pool.acquire().await.unwrap();
    for (ev, why) in [
        (Uuid::new_v4(), "does not exist"),
        (
            evidence(&fx.pool, "public-domain", false).await,
            "not approved",
        ),
        (
            evidence(&fx.pool, "cc-by-nc-4.0", true).await,
            "not in allowed_license_codes",
        ),
    ] {
        let manifest = write_manifest(&fx.dir, ev, "");
        let err = texts::import(
            &mut conn,
            &fx.objects,
            &embedder,
            &manifest,
            &allowed(),
            "o",
            true,
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, TextError::Evidence(ref m) if m.contains(why)),
            "{err}"
        );
    }
    assert_eq!(count(&fx.pool, "text_edition").await, 0);
}

#[tokio::test]
async fn include_sections_restores_an_excluded_section() {
    let fx = start().await;
    let ev = evidence(&fx.pool, "public-domain", true).await;
    let manifest = write_manifest(&fx.dir, ev, r#"include_sections = ["*Introduction"]"#);
    let mut conn = fx.pool.acquire().await.unwrap();
    let report = texts::import(
        &mut conn,
        &fx.objects,
        &FakeEmbedder::new("fake"),
        &manifest,
        &allowed(),
        "o",
        false,
        None,
    )
    .await
    .unwrap();
    assert!(report.sections_excluded.is_empty());
    assert_eq!(report.sections_kept.len(), 3);
}

#[tokio::test]
async fn reembed_renews_stale_vectors_from_stored_text_and_then_does_nothing() {
    let fx = start().await;
    let ev = evidence(&fx.pool, "public-domain", true).await;
    let manifest = write_manifest(&fx.dir, ev, "");
    let embedder = FakeEmbedder::new("fake-bge-m3");
    let mut conn = fx.pool.acquire().await.unwrap();
    texts::import(
        &mut conn,
        &fx.objects,
        &embedder,
        &manifest,
        &allowed(),
        "owner",
        true,
        None,
    )
    .await
    .unwrap();
    let versions: Vec<i32> = sqlx::query_scalar("select vector_version from text_unit")
        .fetch_all(&fx.pool)
        .await
        .unwrap();
    assert_eq!(
        versions,
        vec![wt_ml::VECTOR_VERSION; 3],
        "an import stamps the version"
    );
    assert_eq!(
        texts::reembed(&mut conn, &embedder)
            .await
            .unwrap()
            .reembedded,
        0
    );

    // One unit embedded the old way: a different vector and version 1.
    let snapshot = "select id::text, embedding::text, content_hash from text_unit order by id";
    let before: Vec<(String, String, String)> =
        sqlx::query_as(snapshot).fetch_all(&fx.pool).await.unwrap();
    let edition_before: (Option<String>, String) =
        sqlx::query_as("select units_hash, state::text from text_edition")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    sqlx::query(
        "update text_unit set vector_version = 1, embedding = array_fill(0.5, array[1024])::vector
         where id = $1::uuid",
    )
    .bind(&before[0].0)
    .execute(&fx.pool)
    .await
    .unwrap();

    let report = texts::reembed(&mut conn, &embedder).await.unwrap();
    assert_eq!(report.reembedded, 1);
    let after: Vec<(String, String, String)> =
        sqlx::query_as(snapshot).fetch_all(&fx.pool).await.unwrap();
    assert_eq!(after, before, "the vector comes back from the stored text");
    let edition_after: (Option<String>, String) =
        sqlx::query_as("select units_hash, state::text from text_edition")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(
        edition_after, edition_before,
        "text, hash and state are untouched"
    );
    assert_eq!(
        texts::reembed(&mut conn, &embedder)
            .await
            .unwrap()
            .reembedded,
        0
    );

    // A new model makes every unit stale.
    let other = FakeEmbedder::new("fake-bge-m3-next");
    assert_eq!(
        texts::reembed(&mut conn, &other).await.unwrap().reembedded,
        3
    );
    assert_eq!(
        texts::reembed(&mut conn, &other).await.unwrap().reembedded,
        0
    );
}
