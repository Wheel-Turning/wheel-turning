//! Text takedowns, exclusions, quarantine, source switches and file retention (Spec 08,
//! Mandated sources, Rights lifecycle, Source copies), on synthetic text files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::objects::{LocalObjects, ObjectStore};
use wt_db::test_support::TestDb;
use wt_db::text_takedown::TextScope;
use wt_ingest::config::LicenseSettings;
use wt_ingest::texts::pdf::proof::{Answer, ProofFile};
use wt_ingest::texts::proofread::{self, Thresholds};
use wt_ingest::texts::{self, TextError, takedown};
use wt_ml::FakeEmbedder;

const TEXT: &str = "The first teaching says the mind is like a clear pond on a still morning.

The second teaching says the wind moves the water and the moon breaks into pieces.

The third teaching says the water settles again and the moon is whole as before.
";

struct Fx {
    _db: TestDb,
    pool: PgPool,
    dir: PathBuf,
    objects: LocalObjects,
    evidence: Uuid,
}

async fn start() -> Fx {
    let db = TestDb::start().await.expect("docker must be running");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();
    let dir = std::env::temp_dir().join(format!("wt-text-takedown-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let evidence = sqlx::query_scalar(
        "insert into license_evidence (publisher, license_code, license_url, evidence_url,
                                       evidence_snapshot_ref, snapshot_sha256, captured_at,
                                       reviewed_by, reviewed_at, allowed)
         values ('Fixture Press', 'public-domain', 'https://example.invalid/l',
                 'https://example.invalid/e', 'records/x', 'x', now(), 'reviewer', now(), true)
         returning id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let objects = LocalObjects::new(dir.join("objects"));
    Fx {
        _db: db,
        pool,
        dir,
        objects,
        evidence,
    }
}

impl Fx {
    /// A plain-text edition's manifest in its own folder.
    fn manifest(&self, folder: &str, title: &str, url: &str, source_key: Option<&str>) -> PathBuf {
        let dir = self.dir.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("text.txt"), TEXT).unwrap();
        let key = source_key
            .map(|k| format!("source_key = \"{k}\""))
            .unwrap_or_default();
        let text = format!(
            r#"role = "corpus"

[work]
title = "{title}"

[edition]
label = "Fixture 1902"
rights_holder = "public domain"
license_evidence_id = "{}"
source_url = "{url}"
{key}

[source]
files = ["text.txt"]
format = "txt"

[units]
scheme = "paragraph"
locator_prefix = "fixturetext"
label_template = "{{work}}, paragraph {{n}}"
"#,
            self.evidence
        );
        let path = dir.join("manifest.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    async fn import(&self, manifest: &Path) -> Result<texts::ImportReport, TextError> {
        let mut conn = self.pool.acquire().await.unwrap();
        texts::import(
            &mut conn,
            &self.objects,
            &FakeEmbedder::new("fake"),
            manifest,
            &LicenseSettings::default().allowed_license_codes,
            "owner",
            true,
            None,
        )
        .await
    }

    async fn edition(&self, title: &str) -> Uuid {
        sqlx::query_scalar(
            "select e.id from text_edition e join work w on w.id = e.work_id where w.title = $1",
        )
        .bind(title)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// Proofreads an edition through the sheet, every unit correct.
    async fn proofread(&self, edition: Uuid) -> proofread::ProofReport {
        let mut conn = self.pool.acquire().await.unwrap();
        let tools = NoTools;
        let (_, html) = proofread::sheet(
            &mut conn,
            &self.objects,
            &tools,
            edition,
            &self.dir.join("scratch"),
        )
        .await
        .unwrap();
        let ids = regex::Regex::new(r#"data-unit="([^"]+)""#).unwrap();
        let hash = regex::Regex::new(r#"units_hash: "([^"]*)""#).unwrap();
        let file = ProofFile {
            edition_id: edition,
            units_hash: hash.captures(&html).unwrap()[1].to_owned(),
            answers: ids
                .captures_iter(&html)
                .map(|c| Answer {
                    unit_id: c[1].parse().unwrap(),
                    group: wt_ingest::texts::pdf::proof::Group::Random,
                    errors: 0,
                    note: None,
                })
                .collect(),
        };
        proofread::import_proof(
            &mut conn,
            &self.objects,
            edition,
            &file,
            "Reviewer",
            Thresholds {
                random_max: 1,
                worst_max: 3,
            },
        )
        .await
        .unwrap()
    }

    async fn served(&self) -> i64 {
        sqlx::query_scalar("select count(*) from servable_text_unit")
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn source(&self, key: &str, prefix: &str, retention: &str) {
        sqlx::query(
            "insert into source_policy (key, publisher, url_prefixes, basis, license_evidence_id,
                                        source_retention)
             values ($1, 'Fixture Publisher', array[$2], 'mandate', $3, $4)",
        )
        .bind(key)
        .bind(prefix)
        .bind(self.evidence)
        .bind(retention)
        .execute(&self.pool)
        .await
        .unwrap();
    }
}

/// Plain text has no pages, so the sheet never renders.
struct NoTools;

impl wt_ingest::texts::pdf::tools::PdfTools for NoTools {
    fn page_count(&self, _: &Path) -> Result<u32, TextError> {
        unreachable!()
    }
    fn text_layer(
        &self,
        _: &Path,
        _: u32,
    ) -> Result<wt_ingest::texts::pdf::layout::Page, TextError> {
        unreachable!()
    }
    fn render(
        &self,
        _: &Path,
        _: u32,
        _: u32,
        _: Option<wt_ingest::texts::pdf::tools::Crop>,
        _: &Path,
    ) -> Result<(), TextError> {
        unreachable!()
    }
    fn ocr(
        &self,
        _: &Path,
        _: &[String],
        _: u32,
    ) -> Result<wt_ingest::texts::pdf::layout::Page, TextError> {
        unreachable!()
    }
    fn engine(&self) -> Result<String, TextError> {
        unreachable!()
    }
}

#[tokio::test]
async fn an_edition_takedown_removes_everything_and_nothing_brings_it_back() {
    let fx = start().await;
    let manifest = fx.manifest(
        "first",
        "Pond Fixture",
        "https://texts.example.invalid/pond",
        None,
    );
    fx.import(&manifest).await.unwrap();
    let edition = fx.edition("Pond Fixture").await;
    assert!(fx.proofread(edition).await.ready);
    assert_eq!(fx.served().await, 3);

    let mut conn = fx.pool.acquire().await.unwrap();
    let dry = takedown::take_down(
        &mut conn,
        &fx.objects,
        &TextScope::Edition(edition),
        "rights holder asked",
        "owner",
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        (dry.units, fx.served().await),
        (3, 3),
        "a dry run changes nothing"
    );

    let done = takedown::take_down(
        &mut conn,
        &fx.objects,
        &TextScope::Edition(edition),
        "rights holder asked",
        "owner",
        false,
    )
    .await
    .unwrap();
    assert_eq!(done.units, 3);
    assert_eq!(
        done.exclusions, 6,
        "the work, the edition, three units and the file: {done:?}"
    );
    assert_eq!(
        done.objects_deleted, 3,
        "the text file, files.json and the stored manifest"
    );
    assert_eq!(fx.served().await, 0);
    assert_eq!(fx.count("select count(*) from text_unit").await, 0);
    assert_eq!(
        fx.count("select count(*) from text_edition where state = 'withdrawn' and content_hash is not null").await,
        1,
        "the stub keeps its hashes"
    );
    assert_eq!(
        fx.count("select count(*) from takedown_log where scope = 'edition'")
            .await,
        1
    );
    assert!(
        fx.objects
            .get("texts-source/pond-fixture/fixture-1902/text.txt")
            .unwrap()
            .is_none()
    );
    assert!(
        fx.objects
            .get("texts-work/pond-fixture/fixture-1902/files.json")
            .unwrap()
            .is_none()
    );

    // The same import, the same work under another label, and the same file under another title
    // and address are each refused with the exclusion cited.
    let again = fx.import(&manifest).await.unwrap_err();
    assert!(matches!(again, TextError::Excluded(_)), "{again}");
    let elsewhere = fx.manifest(
        "elsewhere",
        "Another Title",
        "https://mirror.example.invalid/x",
        None,
    );
    let err = fx.import(&elsewhere).await.unwrap_err().to_string();
    assert!(
        err.contains("content") && err.contains("rights holder asked"),
        "{err}"
    );
}

#[tokio::test]
async fn a_source_switch_works_at_once_and_its_takedown_excludes_its_prefix() {
    let fx = start().await;
    fx.source("fixturepress", "https://press.example.invalid/", "keep")
        .await;
    let manifest = fx.manifest(
        "press",
        "Press Fixture",
        "https://press.example.invalid/doc",
        Some("fixturepress"),
    );
    fx.import(&manifest).await.unwrap();
    let edition = fx.edition("Press Fixture").await;
    fx.proofread(edition).await;
    assert_eq!(fx.served().await, 3);

    wt_db::rights::set_enabled(&fx.pool, "fixturepress", false, Some("test"), "owner")
        .await
        .unwrap();
    assert_eq!(
        fx.served().await,
        0,
        "disabling the source stops it at once"
    );
    wt_db::rights::set_enabled(&fx.pool, "fixturepress", true, None, "owner")
        .await
        .unwrap();
    assert_eq!(fx.served().await, 3);

    let mut conn = fx.pool.acquire().await.unwrap();
    let report = takedown::take_down(
        &mut conn,
        &fx.objects,
        &TextScope::Source("fixturepress".into()),
        "publisher declined",
        "owner",
        false,
    )
    .await
    .unwrap();
    assert_eq!(report.editions.len(), 1);
    assert_eq!(fx.served().await, 0);
    assert_eq!(
        fx.count("select count(*) from source_policy where key = 'fixturepress' and not enabled")
            .await,
        1
    );
    assert_eq!(
        fx.count("select count(*) from source_exclusion where scope = 'prefix' and pattern = 'https://press.example.invalid/'").await,
        1
    );
    // A new document under the prefix is refused.
    let other = fx.manifest(
        "other",
        "Other Press Fixture",
        "https://press.example.invalid/other",
        None,
    );
    std::fs::write(
        fx.dir.join("other/text.txt"),
        "A different text entirely, about mountains and rivers far away.\n",
    )
    .unwrap();
    let err = fx.import(&other).await.unwrap_err().to_string();
    assert!(err.contains("press.example.invalid"), "{err}");
    let audit = wt_db::audit::run(
        &fx.pool,
        wt_db::audit::AuditSettings::defaults(wt_core::Env::Prod),
    )
    .await
    .unwrap();
    for name in [
        "text_excluded_served",
        "text_source_served_while_off",
        "text_takedown_completeness",
    ] {
        let c = audit.checks.iter().find(|c| c.name == name).unwrap();
        assert_eq!(c.findings, 0, "{name}");
    }
}

#[tokio::test]
async fn quarantine_stops_serving_and_release_restores_a_proofread_edition() {
    let fx = start().await;
    let manifest = fx.manifest(
        "q",
        "Quarantine Fixture",
        "https://texts.example.invalid/q",
        None,
    );
    fx.import(&manifest).await.unwrap();
    let edition = fx.edition("Quarantine Fixture").await;
    fx.proofread(edition).await;
    let mut conn = fx.pool.acquire().await.unwrap();
    assert_eq!(
        takedown::quarantine(&mut conn, edition, "notice found", "owner")
            .await
            .unwrap(),
        "ready"
    );
    assert_eq!(fx.served().await, 0);
    assert_eq!(
        takedown::release(&mut conn, edition, "owner")
            .await
            .unwrap(),
        "ready"
    );
    assert_eq!(fx.served().await, 3);
    assert_eq!(
        fx.count(
            "select count(*) from correction_log where entity = 'text_edition' and field = 'state'"
        )
        .await,
        2
    );
    assert!(
        takedown::release(&mut conn, edition, "owner")
            .await
            .is_err(),
        "not quarantined"
    );
}

#[tokio::test]
async fn a_mandated_source_keeps_no_originals_after_the_proofread() {
    let fx = start().await;
    fx.source(
        "mandated",
        "https://mandated.example.invalid/",
        "delete_after_proof",
    )
    .await;
    let manifest = fx.manifest(
        "m",
        "Mandated Fixture",
        "https://mandated.example.invalid/doc",
        Some("mandated"),
    );
    fx.import(&manifest).await.unwrap();
    let key = "texts-source/mandated-fixture/fixture-1902/text.txt";
    assert!(fx.objects.get(key).unwrap().is_some());
    let edition = fx.edition("Mandated Fixture").await;
    let report = fx.proofread(edition).await;
    assert!(report.ready && report.source_deleted);
    assert!(fx.objects.get(key).unwrap().is_none());
    assert!(
        fx.objects
            .get("texts-work/mandated-fixture/fixture-1902/files.json")
            .unwrap()
            .is_some(),
        "the hashes stay"
    );
    assert_eq!(
        fx.count("select count(*) from text_edition where source_deleted_at is not null")
            .await,
        1
    );
    assert_eq!(fx.served().await, 3);
}
