//! `wt rights parity`: two databases holding the same talks are compared by keys that survive a
//! copy, and a removal on one side only is reported as unsafe (runbook §12). Two throwaway
//! databases stand in for the PC and RDS. Everything is synthetic.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{ACTOR, ADA, BO, consent, make_ready};
use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::gate::ConsentStatus;
use wt_core::ids::TalkId;
use wt_db::parity::{Kind, ParityReport, run};
use wt_db::takedown::{self, Scope};

const NAMES: (&str, &str) = ("pc", "rds");

/// A talk whose source video has a fixed id, so both databases hold the same key.
async fn keyed_talk(pool: &PgPool, external_id: &str, teacher: &str) -> TalkId {
    let id: Uuid = sqlx::query_scalar(
        "with v as (
           insert into source_video (platform, external_id, url, title)
           values ('youtube', $1, 'https://example.invalid/v', 'A fixture talk') returning id)
         insert into talk (source_video_id, title, teacher_id, sangha_id, rights_holder, state)
         select v.id, 'A fixture talk', te.id, te.sangha_id, te.display_name, 'awaiting_consent'
         from v, teacher te where te.display_name = $2
         returning id",
    )
    .bind(external_id)
    .bind(teacher)
    .fetch_one(pool)
    .await
    .unwrap();
    TalkId::from(id)
}

/// The same content in both: one ready talk each for Ada and Bo.
async fn seed(pool: &PgPool) -> (TalkId, TalkId) {
    let a = keyed_talk(pool, "shared-ada", ADA).await;
    let b = keyed_talk(pool, "shared-bo", BO).await;
    consent(pool, ADA, ConsentStatus::Granted).await;
    consent(pool, BO, ConsentStatus::Granted).await;
    make_ready(pool, a).await;
    make_ready(pool, b).await;
    (a, b)
}

/// `(kind, key, unsafe)` for every difference.
fn found(report: &ParityReport) -> Vec<(Kind, String, bool)> {
    report
        .differences
        .iter()
        .map(|d| (d.kind, d.key.clone(), d.is_unsafe))
        .collect()
}

#[tokio::test]
async fn a_takedown_on_one_side_only_is_unsafe_until_it_is_replayed() {
    let (pc, rds) = tokio::join!(common::start(), common::start());
    let (pc_ada, _) = seed(&pc.pool).await;
    let (rds_ada, _) = seed(&rds.pool).await;

    let report = run(&pc.pool, &rds.pool, NAMES).await.unwrap();
    assert!(report.differences.is_empty(), "{:#?}", report.differences);
    assert!(!report.failed());
    assert_eq!(report.this.talks, 2);
    assert_eq!(report.this.schema_version, report.other.schema_version);

    // Divergence that serves nothing removed: a talk the PC worker found after the copy, and a
    // removal request that reached the beta's form only.
    keyed_talk(&pc.pool, "found-later", BO).await;
    sqlx::query(
        "insert into takedown_request (target, message, status, verified_at)
         values ('a talk url', 'please', 'open', now())",
    )
    .execute(&rds.pool)
    .await
    .unwrap();
    let report = run(&pc.pool, &rds.pool, NAMES).await.unwrap();
    assert!(!report.failed(), "{:#?}", report.differences);
    let benign = found(&report);
    assert!(benign.contains(&(Kind::Talk, "youtube:found-later".into(), false)));
    assert!(
        benign
            .iter()
            .any(|(kind, _, is_unsafe)| *kind == Kind::TakedownRequest && !is_unsafe)
    );
    assert_eq!(report.other.open_takedown_requests, 1);

    // A takedown run from the PC .env only: the beta still serves the talk.
    takedown::run(&pc.pool, &Scope::Talk(pc_ada), ACTOR)
        .await
        .unwrap();
    let report = run(&pc.pool, &rds.pool, NAMES).await.unwrap();
    assert!(report.failed());
    let after = found(&report);
    assert!(after.contains(&(Kind::Takedown, "talk youtube:shared-ada".into(), true)));
    assert!(after.contains(&(Kind::Talk, "youtube:shared-ada".into(), true)));
    let talk = report
        .differences
        .iter()
        .find(|d| d.kind == Kind::Talk && d.key == "youtube:shared-ada")
        .unwrap();
    assert!(talk.note.starts_with("rds serves more"), "{}", talk.note);
    // Unsafe findings come first.
    assert!(report.differences[0].is_unsafe);

    // Replaying it on the beta leaves only the benign differences.
    takedown::run(&rds.pool, &Scope::Talk(rds_ada), ACTOR)
        .await
        .unwrap();
    let report = run(&pc.pool, &rds.pool, NAMES).await.unwrap();
    assert!(!report.failed(), "{:#?}", report.differences);
    assert_eq!(found(&report), benign);
}

#[tokio::test]
async fn opt_outs_settings_and_exclusions_are_compared_on_their_own_rows() {
    let (pc, rds) = tokio::join!(common::start(), common::start());
    seed(&pc.pool).await;
    seed(&rds.pool).await;

    // On the beta only: Ada's opt-out and an exclusion. On the PC only: mandated talks on.
    wt_db::rights::opt_out(&rds.pool, ADA, "asked by email", ACTOR, true)
        .await
        .unwrap();
    let mut conn = rds.pool.acquire().await.unwrap();
    wt_db::rights::add_exclusion(
        &mut conn,
        "prefix",
        "https://example.invalid/withdrawn/",
        "fixture",
        None,
        ACTOR,
    )
    .await
    .unwrap();
    drop(conn);
    wt_db::rights::set_talks_basis(&pc.pool, "mandate", ACTOR)
        .await
        .unwrap();

    let report = run(&pc.pool, &rds.pool, NAMES).await.unwrap();
    assert!(report.failed());
    let got = found(&report);
    assert!(got.contains(&(Kind::Teacher, ADA.into(), true)), "{got:#?}");
    assert!(got.contains(&(Kind::Setting, "talks_basis".into(), true)));
    assert!(got.contains(&(
        Kind::Exclusion,
        "prefix https://example.invalid/withdrawn/".into(),
        true
    )));
    // The opt-out is one finding on the teacher, not one per talk.
    assert!(
        !got.iter().any(|(kind, _, _)| *kind == Kind::Talk),
        "{got:#?}"
    );
    assert_eq!(report.unsafe_differences, 3);

    // The comparison is symmetric: swapping the sides finds the same keys.
    let swapped = run(&rds.pool, &pc.pool, ("rds", "pc")).await.unwrap();
    let mut a: Vec<_> = got.into_iter().map(|(k, key, _)| (k, key)).collect();
    let mut b: Vec<_> = found(&swapped)
        .into_iter()
        .map(|(k, key, _)| (k, key))
        .collect();
    a.sort();
    b.sort();
    assert_eq!(a, b);
}
