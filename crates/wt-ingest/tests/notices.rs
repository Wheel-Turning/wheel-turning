//! Legal pages, the notice scan with each source's notice action, and the English-only rule
//! (Spec 08), on synthetic text files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::objects::LocalObjects;
use wt_db::test_support::TestDb;
use wt_ingest::config::LicenseSettings;
use wt_ingest::texts::{self, ImportReport};
use wt_ml::FakeEmbedder;

const NOTICED: &str = "Copyright 1994 by Fixture Holder Estate. All rights reserved.

The first teaching says the mind is like a clear pond on a still morning.

The second teaching says the wind moves the water and the moon breaks into pieces.

The third teaching says the water settles again and the moon is whole as before.
";

const HUNGARIAN: &str = "A tanítvány reggel leült a fal elé és figyelte a lélegzetét amíg a \
harang háromszor meg nem szólalt a csendes teremben, aztán felállt és kiment a kertbe ahol \
a reggeli fény a köveken feküdt és a madarak énekeltek a fák között a hegy lábánál egészen \
délig minden nap, és a tanító azt mondta hogy ez maga az út.
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
    let dir = std::env::temp_dir().join(format!("wt-notices-{}", Uuid::new_v4()));
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
    async fn import(
        &self,
        folder: &str,
        text: &str,
        holder: &str,
        extra: &str,
        apply: bool,
    ) -> ImportReport {
        let dir = self.dir.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("text.txt"), text).unwrap();
        let manifest = format!(
            r#"role = "corpus"

[work]
title = "{folder} fixture"

[edition]
label = "Fixture 1904"
rights_holder = "{holder}"
license_evidence_id = "{}"
{extra}

[source]
files = ["text.txt"]
format = "txt"

[units]
scheme = "paragraph"
locator_prefix = "{folder}"
label_template = "{{work}}, paragraph {{n}}"
"#,
            self.evidence
        );
        let path = dir.join("manifest.toml");
        std::fs::write(&path, manifest).unwrap();
        let mut conn = self.pool.acquire().await.unwrap();
        texts::import(
            &mut conn,
            &self.objects,
            &FakeEmbedder::new("fake"),
            &path,
            &LicenseSettings::default().allowed_license_codes,
            "owner",
            apply,
            None,
        )
        .await
        .unwrap()
    }

    async fn source(&self, key: &str, action: &str) {
        sqlx::query(
            "insert into source_policy (key, publisher, url_prefixes, basis, license_evidence_id,
                                        notice_action)
             values ($1, 'Fixture Press', array['https://' || $1 || '.example.invalid/'], 'mandate',
                     $2, $3)",
        )
        .bind(key)
        .bind(self.evidence)
        .bind(action)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn editions(&self) -> i64 {
        sqlx::query_scalar("select count(*) from text_edition")
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn a_notice_naming_another_holder_blocks_and_evidence_naming_it_clears() {
    let fx = start().await;
    let dry = fx
        .import("blocked", NOTICED, "public domain", "", false)
        .await;
    assert_eq!(
        dry.notice_outcome.as_deref(),
        Some("blocked: third_party_notice")
    );
    assert_eq!(dry.notices[0].holder, "Fixture Holder Estate");
    let applied = fx
        .import("blocked", NOTICED, "public domain", "", true)
        .await;
    assert!(!applied.applied);
    assert_eq!(fx.editions().await, 0);

    // Evidence naming the holder unblocks it, and the notice stays out of the units.
    let cleared = fx
        .import("cleared", NOTICED, "Fixture Holder Estate", "", true)
        .await;
    assert!(cleared.applied && cleared.notice_outcome.is_none());
    assert_eq!((cleared.units, cleared.legal_blocks), (3, 1));
    let (legal, units): (Option<String>, i64) = sqlx::query_as(
        "select e.legal_text, (select count(*) from text_unit u where u.edition_id = e.id)
         from text_edition e",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert!(legal.unwrap().contains("Fixture Holder Estate"));
    assert_eq!(units, 3);
    let in_units: i64 =
        sqlx::query_scalar("select count(*) from text_unit where text like '%Copyright%'")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(in_units, 0, "a legal page is never embedded or searched");
}

#[tokio::test]
async fn a_mandated_source_flags_holds_or_blocks_noticed_documents() {
    let fx = start().await;
    for action in ["flag", "hold", "block"] {
        fx.source(&format!("{action}press"), action).await;
    }
    let flagged = fx
        .import(
            "flagged",
            NOTICED,
            "public domain",
            "source_key = \"flagpress\"",
            true,
        )
        .await;
    assert_eq!(flagged.notice_outcome.as_deref(), Some("flagged"));
    assert!(flagged.applied);
    let held = fx
        .import(
            "held",
            NOTICED,
            "public domain",
            "source_key = \"holdpress\"",
            true,
        )
        .await;
    assert_eq!(held.notice_outcome.as_deref(), Some("held"));
    let blocked = fx
        .import(
            "refused",
            NOTICED,
            "public domain",
            "source_key = \"blockpress\"",
            true,
        )
        .await;
    assert!(!blocked.applied);

    let mut conn = fx.pool.acquire().await.unwrap();
    let listed = wt_db::text_takedown::noticed(&mut conn, None)
        .await
        .unwrap();
    let states: Vec<(&str, &str)> = listed
        .iter()
        .map(|n| (n.source_key.as_deref().unwrap(), n.state.as_str()))
        .collect();
    assert_eq!(
        states,
        vec![("flagpress", "imported"), ("holdpress", "quarantined")]
    );
    assert!(
        listed[0]
            .notice
            .as_deref()
            .unwrap()
            .contains("Fixture Holder Estate")
    );
    assert_eq!(fx.editions().await, 2);
}

#[tokio::test]
async fn a_text_that_is_not_english_is_skipped_and_reported() {
    let fx = start().await;
    let report = fx
        .import("magyar", HUNGARIAN, "public domain", "", true)
        .await;
    assert!(!report.applied);
    assert!(report.skipped.unwrap().contains("not English"));
    assert_eq!(fx.editions().await, 0);
    let declared = fx
        .import(
            "declared",
            NOTICED,
            "Fixture Holder Estate",
            "language = \"hu\"",
            true,
        )
        .await;
    assert!(
        declared.skipped.is_some(),
        "a manifest that names another language is skipped too"
    );
}
