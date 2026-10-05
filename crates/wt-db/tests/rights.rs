//! Spec 08, Mandated sources and Rights lifecycle: sources on a mandate, the ledger, permission
//! requests and their answers, and permanent exclusions. Every source, address and statement is
//! invented.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use chrono::NaiveDate;
use sqlx::PgPool;
use wt_core::records::{StoredRecord, sha256_hex};
use wt_db::rights::{
    self, Basis, NewBasis, NewSource, RightsError, add_source, decline, due, grant, ledger,
    lift_exclusion, no_response, request, sent,
};

const OWNER: &str = "Owner Fixture";

fn record(text: &str) -> StoredRecord {
    let sha256 = sha256_hex(text.as_bytes());
    StoredRecord {
        key: format!("records/{sha256}"),
        sha256,
    }
}

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

async fn mandate(
    pool: &PgPool,
    key: &str,
    prefixes: &[&str],
) -> Result<sqlx::types::Uuid, RightsError> {
    let prefixes: Vec<String> = prefixes.iter().map(|p| (*p).to_owned()).collect();
    let decision = record(&format!("use {key}"));
    add_source(
        pool,
        &NewSource {
            key,
            publisher: "Fixture Press",
            prefixes: &prefixes,
            basis: NewBasis::Mandate {
                by: OWNER,
                rationale: "free to download; widely shared",
                display_notice: "Excerpted with attribution from {title}, {publisher}.",
                decision: &decision,
            },
        },
    )
    .await
}

async fn events(pool: &PgPool, key: &str) -> Vec<String> {
    ledger(pool, Some(key))
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect()
}

async fn request_status(pool: &PgPool, key: &str) -> String {
    sqlx::query_scalar(
        "select status from permission_request where source_key = $1 order by created_at desc limit 1",
    )
    .bind(key)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn a_mandate_records_its_evidence_policy_and_event() {
    let fx = common::start().await;
    let evidence = mandate(&fx.pool, "fixturepress", &["https://Press.Example/texts/"])
        .await
        .unwrap();

    let (kind, code, allowed, by, notice, prefixes): (
        String,
        String,
        bool,
        String,
        String,
        Vec<String>,
    ) = sqlx::query_as(
        "select kind, license_code, allowed, mandated_by, display_notice, url_prefixes
             from license_evidence where id = $1",
    )
    .bind(evidence)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(kind, "mandate");
    assert_eq!(code, rights::MANDATE_CODE);
    assert!(allowed, "the owner's decision is the approval");
    assert_eq!(by, OWNER);
    assert!(notice.contains("{publisher}"));
    assert_eq!(
        prefixes,
        ["https://press.example/texts/"],
        "host lowercased"
    );

    let (basis, enabled): (String, bool) =
        sqlx::query_as("select basis, enabled from source_policy where key = 'fixturepress'")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!((basis.as_str(), enabled), ("mandate", true));
    assert_eq!(events(&fx.pool, "fixturepress").await, ["mandated"]);
    assert!(matches!(
        mandate(&fx.pool, "fixturepress", &["https://other.example/"]).await,
        Err(RightsError::SourceExists(_))
    ));
    assert!(matches!(
        mandate(&fx.pool, "ftp", &["ftp://press.example/"]).await,
        Err(RightsError::BadPrefix(_))
    ));
}

#[tokio::test]
async fn a_request_is_sent_answered_and_read_back_in_order() {
    let fx = common::start().await;
    mandate(&fx.pool, "fixturepress", &["https://press.example/"])
        .await
        .unwrap();

    request(
        &fx.pool,
        "fixturepress",
        "rights@press.example",
        "email",
        &record("draft"),
        OWNER,
    )
    .await
    .unwrap();
    assert_eq!(request_status(&fx.pool, "fixturepress").await, "draft");
    let follow_up = sent(&fx.pool, "fixturepress", day(2026, 10, 6), OWNER)
        .await
        .unwrap();
    assert_eq!(follow_up, day(2026, 10, 20));
    assert_eq!(request_status(&fx.pool, "fixturepress").await, "sent");

    let evidence = grant(
        &fx.pool,
        "fixturepress",
        &record("Yes, with attribution."),
        "Fixture Press rights desk",
        day(2026, 10, 12),
        "excerpting with attribution",
        OWNER,
    )
    .await
    .unwrap();
    assert_eq!(request_status(&fx.pool, "fixturepress").await, "granted");
    let (basis, linked): (String, sqlx::types::Uuid) = sqlx::query_as(
        "select basis, license_evidence_id from source_policy where key = 'fixturepress'",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!((basis.as_str(), linked), ("permission", evidence));
    let (kind, grantor, allowed): (String, String, bool) =
        sqlx::query_as("select kind, grantor, allowed from license_evidence where id = $1")
            .bind(evidence)
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(
        (kind.as_str(), grantor.as_str(), allowed),
        ("permission", "Fixture Press rights desk", true)
    );

    assert_eq!(
        events(&fx.pool, "fixturepress").await,
        [
            "mandated",
            "request_drafted",
            "request_sent",
            "permission_granted"
        ],
        "one per step, in order"
    );
    let all = ledger(&fx.pool, None).await.unwrap();
    assert!(all.windows(2).all(|w| w[0].at <= w[1].at));
    assert_eq!(all[2].evidence_ref, None);
    assert_eq!(
        all[3].evidence_ref,
        Some(record("Yes, with attribution.").key)
    );
}

#[tokio::test]
async fn a_decline_disables_the_source_and_excludes_it_until_a_grant_lifts_it() {
    let fx = common::start().await;
    mandate(
        &fx.pool,
        "fixturepress",
        &["https://press.example/texts/", "https://press.example/pdf/"],
    )
    .await
    .unwrap();
    request(
        &fx.pool,
        "fixturepress",
        "rights@press.example",
        "form",
        &record("draft"),
        OWNER,
    )
    .await
    .unwrap();
    sent(&fx.pool, "fixturepress", day(2026, 10, 6), OWNER)
        .await
        .unwrap();

    let out = decline(&fx.pool, "fixturepress", &record("Please do not."), OWNER)
        .await
        .unwrap();
    assert_eq!(out.exclusions.len(), 2, "one per prefix");
    assert_eq!(request_status(&fx.pool, "fixturepress").await, "declined");
    let enabled: bool =
        sqlx::query_scalar("select enabled from source_policy where key = 'fixturepress'")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert!(!enabled);
    assert_eq!(
        events(&fx.pool, "fixturepress").await,
        [
            "mandated",
            "request_drafted",
            "request_sent",
            "permission_declined",
            "disabled",
            "exclusion_added",
            "exclusion_added"
        ]
    );

    // The same prefix, a narrower one and a broader one are all refused, citing the exclusion.
    for prefix in [
        "https://press.example/texts/",
        "https://PRESS.example/texts/volume-1/",
        "https://press.example/",
    ] {
        match mandate(&fx.pool, "again", &[prefix]).await {
            Err(RightsError::Excluded(x)) => {
                assert!(out.exclusions.contains(&x.id), "{prefix}");
                assert!(
                    RightsError::Excluded(x)
                        .to_string()
                        .contains("refused by exclusion")
                );
            }
            other => panic!("{prefix}: {other:?}"),
        }
    }
    assert!(
        mandate(&fx.pool, "elsewhere", &["https://press.example/blog/"])
            .await
            .is_ok()
    );

    // Only the rights holder's written grant lifts an exclusion, and the lift is logged.
    for id in &out.exclusions {
        lift_exclusion(&fx.pool, *id, OWNER, &record("You may use it after all."))
            .await
            .unwrap();
    }
    assert!(matches!(
        lift_exclusion(&fx.pool, out.exclusions[0], OWNER, &record("again")).await,
        Err(RightsError::UnknownExclusion(_))
    ));
    assert_eq!(
        events(&fx.pool, "fixturepress")
            .await
            .iter()
            .filter(|e| *e == "exclusion_lifted")
            .count(),
        2
    );
    assert!(
        mandate(&fx.pool, "again", &["https://press.example/texts/"])
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn silence_stays_a_mandate_and_due_lists_what_needs_doing() {
    let fx = common::start().await;
    mandate(&fx.pool, "quiet", &["https://quiet.example/"])
        .await
        .unwrap();
    mandate(&fx.pool, "drafted", &["https://drafted.example/"])
        .await
        .unwrap();
    request(
        &fx.pool,
        "quiet",
        "hello@quiet.example",
        "email",
        &record("q"),
        OWNER,
    )
    .await
    .unwrap();
    sent(&fx.pool, "quiet", day(2026, 10, 1), OWNER)
        .await
        .unwrap();
    request(
        &fx.pool,
        "drafted",
        "hello@drafted.example",
        "email",
        &record("d"),
        OWNER,
    )
    .await
    .unwrap();

    let before: Vec<_> = due(&fx.pool, day(2026, 10, 14)).await.unwrap();
    assert_eq!(
        before
            .iter()
            .map(|d| (d.source_key.as_str(), d.why))
            .collect::<Vec<_>>(),
        [("drafted", "unsent")],
        "the sent request is not due until its follow-up date"
    );
    let after = due(&fx.pool, day(2026, 10, 15)).await.unwrap();
    assert!(
        after
            .iter()
            .any(|d| d.source_key == "quiet" && d.why == "follow_up_due")
    );

    no_response(&fx.pool, "quiet", OWNER).await.unwrap();
    assert_eq!(request_status(&fx.pool, "quiet").await, "no_response");
    let basis: String = sqlx::query_scalar("select basis from source_policy where key = 'quiet'")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(
        Basis::parse(&basis),
        Some(Basis::Mandate),
        "silence is never permission"
    );
    assert!(
        events(&fx.pool, "quiet")
            .await
            .contains(&"no_response_noted".to_owned())
    );
    assert!(matches!(
        no_response(&fx.pool, "drafted", OWNER).await,
        Err(RightsError::NoRequest { wanted: "sent", .. })
    ));
}

#[tokio::test]
async fn the_ledger_is_append_only_and_a_permission_needs_its_scope() {
    let fx = common::start().await;
    mandate(&fx.pool, "fixturepress", &["https://press.example/"])
        .await
        .unwrap();
    for sql in [
        "update rights_event set actor = 'someone else'",
        "delete from rights_event",
        "truncate rights_event",
    ] {
        let err = sqlx::query(sql).execute(&fx.pool).await.unwrap_err();
        assert!(err.to_string().contains("append-only"), "{sql}: {err}");
    }
    let err = sqlx::query(
        "insert into license_evidence (publisher, license_code, evidence_snapshot_ref,
           snapshot_sha256, captured_at, kind)
         values ('Fixture Press', 'permission-excerpt', 'records/x', 'x', now(), 'permission')",
    )
    .execute(&fx.pool)
    .await
    .unwrap_err();
    assert!(err.to_string().contains("permission_has_scope"), "{err}");
    let err = sqlx::query(
        "insert into license_evidence (license_code, evidence_snapshot_ref, snapshot_sha256,
           captured_at) values ('cc0', 'records/x', 'x', now())",
    )
    .execute(&fx.pool)
    .await
    .unwrap_err();
    assert!(err.to_string().contains("license_evidence_owner"), "{err}");
}
