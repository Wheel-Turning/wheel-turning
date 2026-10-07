//! Spec 08, reference assets and citation links: `wt reference import` for glossaries, alias
//! lists, lineage and locator lists, `wt text link-out`, and `wt reindex --citations`'s link
//! from citations to text units. Everything is synthetic.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::objects::LocalObjects;
use wt_db::test_support::TestDb;
use wt_ingest::config::LicenseSettings;
use wt_ingest::texts::reference;
use wt_registry::RegistryFile;
use wt_registry::glossary::{Glossary, GlossaryTerm, TermKind};

const REGISTRY: &str = r#"
[[sangha]]
name = "Fixture Mountain Zendo"

[[teacher]]
display_name = "Ada Fixture"
sangha = "Fixture Mountain Zendo"
aliases = ["Ada"]

[[teacher]]
display_name = "Bo Sample"
sangha = "Fixture Mountain Zendo"
aliases = ["Bo"]
"#;

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
    wt_db::registry::import(
        &pool,
        &RegistryFile::parse(REGISTRY, "fixture").unwrap(),
        "test",
    )
    .await
    .unwrap();
    let dir = std::env::temp_dir().join(format!("wt-reference-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let objects = LocalObjects::new(dir.join("objects"));
    Fx {
        _db: db,
        pool,
        dir,
        objects,
    }
}

/// Writes a manifest and its CSV into a folder of their own.
fn asset(dir: &Path, name: &str, kind: &str, csv: &str, columns: &str) -> PathBuf {
    let folder = dir.join(name);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("rows.csv"), csv).unwrap();
    let manifest = folder.join("manifest.toml");
    std::fs::write(
        &manifest,
        format!(
            "role = \"reference\"\n\n[asset]\nkind = \"{kind}\"\ntitle = \"Fixture {name}\"\n\n\
             [source]\nfiles = [\"rows.csv\"]\nformat = \"csv\"\n{columns}\n"
        ),
    )
    .unwrap();
    manifest
}

async fn run(
    fx: &Fx,
    manifest: &Path,
    link_teachers: bool,
) -> wt_ingest::texts::reference::ReferenceReport {
    let mut conn = fx.pool.acquire().await.unwrap();
    reference::import(
        &mut conn,
        &fx.objects,
        manifest,
        &LicenseSettings::default().allowed_license_codes,
        "owner",
        true,
        link_teachers,
    )
    .await
    .unwrap()
    .0
}

async fn scalar(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_glossary_merges_variants_reports_conflicts_and_changes_the_version() {
    let fx = start().await;
    let mut conn = fx.pool.acquire().await.unwrap();
    wt_db::glossary::import(
        &mut conn,
        &[GlossaryTerm {
            canonical: "Dogen".into(),
            kind: TermKind::Name,
            variants: vec!["doe gen".into()],
        }],
        "seed",
    )
    .await
    .unwrap();
    let before = Glossary::new(&wt_db::glossary::load(&mut conn).await.unwrap())
        .unwrap()
        .version()
        .to_owned();
    let manifest = asset(
        &fx.dir,
        "glossary",
        "glossary",
        "term,mishearings,type\nDogen,dough gen;doe gen,name\nShikantaza,she can taza,term\nDōgen,doagen,name\n",
        "columns = { canonical = \"term\", variants = \"mishearings\", kind = \"type\" }",
    );
    let report = run(&fx, &manifest, false).await;
    assert_eq!(report.counts["terms_added"], 1, "{report:?}");
    assert_eq!(report.counts["variants_added"], 1);
    assert_eq!(
        report.conflicts.len(),
        1,
        "Dōgen differs from Dogen only in accents"
    );
    let stored = wt_db::glossary::load(&mut conn).await.unwrap();
    let dogen = stored.iter().find(|t| t.canonical == "Dogen").unwrap();
    assert!(dogen.variants.contains(&"dough gen".to_owned()));
    assert!(
        !stored.iter().any(|t| t.canonical == "Dōgen"),
        "never overwritten silently"
    );
    let after = Glossary::new(&stored).unwrap().version().to_owned();
    assert_ne!(
        before, after,
        "affected talks requeue on the version change"
    );
    assert_eq!(
        scalar(
            &fx.pool,
            "select count(*) from glossary_term where source_asset_id is not null"
        )
        .await,
        1,
        "the new term records its asset; Dogen keeps its earlier source"
    );
    let again = run(&fx, &manifest, false).await;
    assert!(again.unchanged && !again.applied);
}

#[tokio::test]
async fn an_alias_list_makes_persons_and_never_teacher_aliases() {
    let fx = start().await;
    let teacher_aliases = scalar(&fx.pool, "select count(*) from teacher_alias").await;
    let manifest = asset(
        &fx.dir,
        "aliases",
        "alias_list",
        "name,aliases,kind\nEihei Dogen,Dogen;Dogen Zenji,author\nHuineng,Hui-neng;Ada,figure\n",
        "",
    );
    let report = run(&fx, &manifest, false).await;
    assert_eq!((report.counts["persons"], report.counts["aliases"]), (2, 4));
    assert_eq!(
        scalar(&fx.pool, "select count(*) from teacher_alias").await,
        teacher_aliases,
        "teacher aliases come only from the registry"
    );
    assert!(report.conflicts.is_empty());
    let clash = asset(
        &fx.dir,
        "clash",
        "alias_list",
        "name,aliases\nOther Person,Dogen\n",
        "",
    );
    let report = run(&fx, &clash, false).await;
    assert_eq!(report.conflicts.len(), 1, "{report:?}");
}

#[tokio::test]
async fn lineage_makes_edges_and_links_teachers_only_by_exact_name_when_asked() {
    let fx = start().await;
    let rows = "parent,child,kind\nFixture Ancestor,Ada Fixture,transmission\nFixture Ancestor,bo sample,teacher_student\n";
    let without = asset(&fx.dir, "lineage1", "lineage", rows, "");
    let report = run(&fx, &without, false).await;
    assert_eq!((report.counts["persons"], report.counts["edges"]), (3, 2));
    assert!(report.teachers_linked.is_empty());
    assert_eq!(
        scalar(
            &fx.pool,
            "select count(*) from teacher where person_id is not null"
        )
        .await,
        0
    );

    let with = asset(
        &fx.dir,
        "lineage2",
        "lineage",
        &format!("{rows}Ada Fixture,Fixture Heir,teacher_student\n"),
        "",
    );
    let report = run(&fx, &with, true).await;
    assert_eq!(
        report.teachers_linked,
        vec!["Ada Fixture"],
        "bo sample is not an exact match"
    );
    assert_eq!(
        report.counts["edges"], 1,
        "the two earlier edges exist already"
    );
}

#[tokio::test]
async fn a_locator_list_and_a_link_out_make_works_and_locators_with_no_text() {
    let fx = start().await;
    let list = asset(
        &fx.dir,
        "locators",
        "locator_list",
        "work,locator,label,aliases,url\nGenjokoan,genjokoan:5,\"Genjokoan, paragraph 5\",To study the self,\nGenjokoan,genjokoan:6,\"Genjokoan, paragraph 6\",,\n",
        "",
    );
    let report = run(&fx, &list, false).await;
    assert_eq!((report.counts["works"], report.counts["locators"]), (1, 2));
    let aliases: Vec<String> =
        sqlx::query_scalar("select aliases from work_locator where locator = 'genjokoan:5'")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(aliases, vec!["To study the self"]);

    let csv = fx.dir.join("out.csv");
    std::fs::write(
        &csv,
        "locator,label\nfixture-press:1,\"Fixture Press, chapter 1\"\n",
    )
    .unwrap();
    let mut conn = fx.pool.acquire().await.unwrap();
    let n = reference::link_out(
        &mut conn,
        "Restricted Fixture",
        "https://example.invalid/book",
        Some(&csv),
    )
    .await
    .unwrap();
    assert_eq!(n, 1);
    let (url, locator_url): (String, String) = sqlx::query_as(
        "select w.link_out_url, wl.link_out_url from work w join work_locator wl on wl.work_id = w.id
         where w.title = 'Restricted Fixture'",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(
        (url.as_str(), locator_url.as_str()),
        (
            "https://example.invalid/book",
            "https://example.invalid/book"
        )
    );
    assert_eq!(
        scalar(&fx.pool, "select count(*) from text_edition").await,
        0
    );
    assert_eq!(scalar(&fx.pool, "select count(*) from text_unit").await, 0);
}

/// A talk's resolved citation of `genjokoan:5`, written as the index step would.
async fn cited_talk(pool: &PgPool) -> Uuid {
    sqlx::query_scalar(
        "with v as (insert into source_video (platform, external_id, url, title)
                    values ('vimeo', '1', 'https://vimeo.com/1', 'Fixture') returning id),
              t as (insert into talk (source_video_id, title, state)
                    select v.id, 'Fixture', 'discovered' from v returning id),
              tr as (insert into transcript (talk_id, version, origin, s3_key, content_hash)
                     select t.id, 1, 'asr', 'k', 'h' from t returning id, talk_id),
              s as (insert into segment (talk_id, transcript_id, ordinal, kind, start_ms, end_ms, text)
                    select tr.talk_id, tr.id, 0, 'talk', 0, 9000, 'x' from tr returning id, talk_id),
              p as (insert into passage (talk_id, segment_id, segment_kind, ordinal, start_ms, end_ms,
                                         text, word_count, content_hash, embedding, embedding_model)
                    select s.talk_id, s.id, 'talk', 0, 0, 9000,
                           'To study the way is to study the self, as the teacher said.', 12, 'h',
                           array_fill(0.03125::real, array[1024])::vector, 'fake' from s
                    returning id, talk_id)
         insert into citation (passage_id, talk_id, raw_text, work_locator_id, confidence, method)
         select p.id, p.talk_id, 'to study the self', wl.id, 'resolved', 'hand_list'
         from p, work_locator wl where wl.locator = 'genjokoan:5'
         returning id",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn reindexing_citations_links_a_resolved_locator_to_its_text_unit() {
    let fx = start().await;
    let list = asset(
        &fx.dir,
        "locators",
        "locator_list",
        "work,locator,label\nGenjokoan,genjokoan:5,\"Genjokoan, paragraph 5\"\n",
        "",
    );
    run(&fx, &list, false).await;
    let citation = cited_talk(&fx.pool).await;

    // An edition of the work with a unit at that locator.
    let evidence: Uuid = sqlx::query_scalar(
        "insert into license_evidence (publisher, license_code, license_url, evidence_url,
                                       evidence_snapshot_ref, snapshot_sha256, captured_at,
                                       reviewed_by, reviewed_at, allowed)
         values ('Fixture Press', 'public-domain', 'https://example.invalid/l',
                 'https://example.invalid/e', 'records/x', 'x', now(), 'r', now(), true)
         returning id",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    let edition: Uuid = sqlx::query_scalar(
        "insert into text_edition (work_id, edition_label, rights_holder, license_evidence_id,
                                   excerptable, state)
         select id, 'Fixture 1901', 'public domain', $1, true, 'ready' from work
         where title = 'Genjokoan' returning id",
    )
    .bind(evidence)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    let unit: Uuid = sqlx::query_scalar(
        "insert into text_unit (edition_id, ordinal, locator, label, text, word_count, content_hash,
                                embedding, embedding_model)
         values ($1, 0, 'genjokoan:5', 'Genjokoan, paragraph 5',
                 'To study the way is to study the self.', 9, 'h',
                 array_fill(0.03125::real, array[1024])::vector, 'fake') returning id",
    )
    .bind(edition)
    .fetch_one(&fx.pool)
    .await
    .unwrap();

    let mut conn = fx.pool.acquire().await.unwrap();
    assert_eq!(
        wt_db::reference::link_citations(&mut conn).await.unwrap(),
        1
    );
    let linked: Option<Uuid> =
        sqlx::query_scalar("select text_unit_id from citation where id = $1")
            .bind(citation)
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(linked, Some(unit));
    assert_eq!(
        wt_db::reference::link_citations(&mut conn).await.unwrap(),
        0,
        "a rerun changes nothing"
    );

    // An edition taken out of service unlinks.
    sqlx::query("update text_edition set state = 'quarantined' where id = $1")
        .bind(edition)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(
        wt_db::reference::link_citations(&mut conn).await.unwrap(),
        1
    );
    let linked: Option<Uuid> =
        sqlx::query_scalar("select text_unit_id from citation where id = $1")
            .bind(citation)
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(linked, None);
}
