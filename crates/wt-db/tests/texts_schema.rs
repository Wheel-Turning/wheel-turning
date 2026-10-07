//! Spec 08 (M4b), the texts schema: the `ready_requires_license` check, every condition of
//! `servable_text_unit`, and what the web role may read. Everything is synthetic.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use sqlx::PgPool;
use sqlx::types::Uuid;

/// Approved public-domain evidence for a fixture publisher.
async fn evidence(pool: &PgPool, allowed: bool) -> Uuid {
    sqlx::query_scalar(
        "insert into license_evidence (publisher, license_code, license_url, evidence_url,
                                       evidence_snapshot_ref, snapshot_sha256, captured_at,
                                       reviewed_by, reviewed_at, allowed)
         values ('Fixture Press', 'public-domain', 'https://example.invalid/pd',
                 'https://example.invalid/e', 'records/x', 'x', now(), 'reviewer', now(), $1)
         returning id",
    )
    .bind(allowed)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A work with one edition holding one unit, in the given state.
async fn edition(pool: &PgPool, evidence: Uuid, excerptable: bool) -> (Uuid, Uuid) {
    let work: Uuid = sqlx::query_scalar(
        "insert into work (title) values ('Fixture Sutra ' || gen_random_uuid()) returning id",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let edition: Uuid = sqlx::query_scalar(
        "insert into text_edition (work_id, edition_label, rights_holder, license_evidence_id,
                                   excerptable)
         values ($1, 'Fixture 1901', 'Public domain', $2, $3) returning id",
    )
    .bind(work)
    .bind(evidence)
    .bind(excerptable)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into text_unit (edition_id, ordinal, locator, label, text, word_count,
                                content_hash, embedding, embedding_model)
         values ($1, 0, 'fixture:1', 'Fixture Sutra, verse 1',
                 'All conditioned things are impermanent, the fixture says.', 8, 'h',
                 array_fill(0.03125::real, array[1024])::vector, 'fake')",
    )
    .bind(edition)
    .execute(pool)
    .await
    .unwrap();
    (work, edition)
}

async fn served(pool: &PgPool, edition: Uuid) -> i64 {
    sqlx::query_scalar("select count(*) from servable_text_unit where edition_id = $1")
        .bind(edition)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn proofread(pool: &PgPool, edition: Uuid, passed: bool) {
    sqlx::query(
        "insert into proof_check (edition_id, sample_size, errors, reviewer, passed)
         values ($1, 24, 0, 'reviewer', $2)",
    )
    .bind(edition)
    .bind(passed)
    .execute(pool)
    .await
    .unwrap();
}

async fn set_ready(pool: &PgPool, edition: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("update text_edition set state = 'ready' where id = $1")
        .bind(edition)
        .execute(pool)
        .await
        .map(|_| ())
}

#[tokio::test]
async fn a_ready_edition_needs_evidence_excerptable_and_a_rights_holder() {
    let fx = common::start().await;
    let ev = evidence(&fx.pool, true).await;
    let (_, not_excerptable) = edition(&fx.pool, ev, false).await;
    assert!(set_ready(&fx.pool, not_excerptable).await.is_err());

    let (_, no_holder) = edition(&fx.pool, ev, true).await;
    sqlx::query("update text_edition set rights_holder = null where id = $1")
        .bind(no_holder)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert!(set_ready(&fx.pool, no_holder).await.is_err());

    let (_, no_evidence) = edition(&fx.pool, ev, true).await;
    sqlx::query("update text_edition set license_evidence_id = null where id = $1")
        .bind(no_evidence)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert!(set_ready(&fx.pool, no_evidence).await.is_err());

    let (_, good) = edition(&fx.pool, ev, true).await;
    assert!(set_ready(&fx.pool, good).await.is_ok());
}

#[tokio::test]
async fn servable_text_units_need_ready_allowed_excerptable_proofread_and_an_enabled_source() {
    let fx = common::start().await;
    let ev = evidence(&fx.pool, true).await;
    let (_, e) = edition(&fx.pool, ev, true).await;
    proofread(&fx.pool, e, true).await;
    assert_eq!(served(&fx.pool, e).await, 0, "imported, not ready");
    set_ready(&fx.pool, e).await.unwrap();
    assert_eq!(served(&fx.pool, e).await, 1);

    // Evidence withdrawn.
    sqlx::query("update license_evidence set allowed = false where id = $1")
        .bind(ev)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(served(&fx.pool, e).await, 0);
    sqlx::query("update license_evidence set allowed = true where id = $1")
        .bind(ev)
        .execute(&fx.pool)
        .await
        .unwrap();

    // No passing proofread.
    let (_, unproofed) = edition(&fx.pool, ev, true).await;
    set_ready(&fx.pool, unproofed).await.unwrap();
    proofread(&fx.pool, unproofed, false).await;
    assert_eq!(served(&fx.pool, unproofed).await, 0);
    proofread(&fx.pool, unproofed, true).await;
    assert_eq!(served(&fx.pool, unproofed).await, 1);

    // A disabled source.
    sqlx::query(
        "insert into source_policy (key, publisher, url_prefixes, basis, license_evidence_id)
         values ('fixturepress', 'Fixture Press', '{https://example.invalid/}', 'license', $1)",
    )
    .bind(ev)
    .execute(&fx.pool)
    .await
    .unwrap();
    sqlx::query("update text_edition set source_key = 'fixturepress' where id = $1")
        .bind(e)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(served(&fx.pool, e).await, 1);
    sqlx::query("update source_policy set enabled = false where key = 'fixturepress'")
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(served(&fx.pool, e).await, 0);
}

/// What `wt_web` may read: the serving view and the person tables, never the text tables.
#[tokio::test]
async fn the_web_role_reads_the_view_and_persons_and_nothing_else_of_the_texts() {
    let fx = common::start().await;
    for (sql, allowed) in [
        ("select count(*) from servable_text_unit", true),
        ("select count(*) from person", true),
        ("select count(*) from person_alias", true),
        ("select count(*) from text_unit", false),
        ("select count(*) from text_edition", false),
        ("select count(*) from reference_asset", false),
        ("select count(*) from proof_check", false),
    ] {
        let mut tx = fx.pool.begin().await.unwrap();
        sqlx::query("set local role wt_web")
            .execute(&mut *tx)
            .await
            .unwrap();
        let result = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql))
            .fetch_one(&mut *tx)
            .await;
        assert_eq!(result.is_ok(), allowed, "{sql}: {result:?}");
        tx.rollback().await.unwrap();
    }
    // The web role may file a takedown request, and read none back.
    let mut tx = fx.pool.begin().await.unwrap();
    sqlx::query("set local role wt_web")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("insert into takedown_request (target) values ('https://example.invalid/x')")
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(
        sqlx::query("select count(*) from takedown_request")
            .execute(&mut *tx)
            .await
            .is_err()
    );
}
