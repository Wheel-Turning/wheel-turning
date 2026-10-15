//! Spec 03, Mandated talks: the talks_basis switch, clearing on a talk source's mandate, the
//! constraint and the gate, teacher opt-out, and consent answers that change the basis.
//! Every teacher, source and statement is invented.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{ACTOR, ADA, BO, count, make_ready, servable, state, state_reason, talk};
use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::gate::{ConsentStatus, GateError};
use wt_core::ids::TalkId;
use wt_core::records::{StoredRecord, sha256_hex};
use wt_db::rights::{
    self, NewBasis, NewSource, Subject, add_source, clear_waiting, ledger, opt_out, set_talks_basis,
};
use wt_db::talk::TalkRepo;

const SOURCE: &str = "fixture-zendo-talks";

fn record(text: &str) -> StoredRecord {
    let sha256 = sha256_hex(text.as_bytes());
    StoredRecord {
        key: format!("records/{sha256}"),
        sha256,
    }
}

/// A talks source on the owner's mandate.
async fn talks_source(pool: &PgPool) -> Uuid {
    let prefixes = vec!["https://video.example/fixturezendo/".to_owned()];
    add_source(
        pool,
        &NewSource {
            key: SOURCE,
            publisher: "Fixture Mountain Zendo",
            prefixes: &prefixes,
            kind: "talks",
            basis: NewBasis::Mandate {
                by: "Owner Fixture",
                rationale: "talks the sangha publishes openly",
                display_notice: "Talk by {teacher}, {sangha}.",
                decision: &record("use the zendo's talks"),
            },
        },
    )
    .await
    .unwrap()
}

/// A talk waiting at the gate, credited to `teacher`, from the mandated source.
async fn mandated_talk(pool: &PgPool, teacher: &str) -> TalkId {
    let t = talk(pool, Some(teacher), Some(teacher)).await;
    sqlx::query("update talk set source_key = $2 where id = $1")
        .bind(t.as_uuid())
        .bind(SOURCE)
        .execute(pool)
        .await
        .unwrap();
    t
}

async fn basis_of(pool: &PgPool, t: TalkId) -> Option<String> {
    sqlx::query_scalar("select rights_basis::text from talk where id = $1")
        .bind(t.as_uuid())
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn gate(pool: &PgPool, t: TalkId) -> Result<(), GateError> {
    let mut conn = pool.acquire().await.unwrap();
    TalkRepo::authorize(&mut conn, t).await.unwrap().map(|_| ())
}

#[tokio::test]
async fn a_mandated_talk_waits_while_the_switch_is_off_and_clears_without_consent_when_on() {
    let fx = common::start().await;
    let evidence = talks_source(&fx.pool).await;
    let t = mandated_talk(&fx.pool, ADA).await;

    // A fresh database is consent-only: the talk waits, and says why.
    let mut conn = fx.pool.acquire().await.unwrap();
    assert!(
        clear_waiting(&mut conn, SOURCE, ACTOR)
            .await
            .unwrap()
            .is_empty()
    );
    drop(conn);
    assert_eq!(state(&fx.pool, t).await, "awaiting_consent");
    assert_eq!(
        state_reason(&fx.pool, t).await.as_deref(),
        Some("mandate_off")
    );

    let on = set_talks_basis(&fx.pool, "mandate", "Owner Fixture")
        .await
        .unwrap();
    assert!(on.changed);
    assert_eq!(on.cleared, vec![t], "switching on clears the waiting talk");
    assert_eq!(basis_of(&fx.pool, t).await.as_deref(), Some("mandate"));
    let (linked, consent_rows): (Uuid, i64) = sqlx::query_as(
        "select t.license_evidence_id,
                (select count(*) from speaker_consent sc where sc.teacher_id = t.teacher_id)
         from talk t where t.id = $1",
    )
    .bind(t.as_uuid())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(linked, evidence, "the source's mandate evidence");
    assert_eq!(consent_rows, 0, "no consent row was needed");

    // Ready and served while on; hidden, not deleted, while off.
    make_ready(&fx.pool, t).await;
    assert_eq!(servable(&fx.pool).await, 1);
    let off = set_talks_basis(&fx.pool, "consent", "Owner Fixture")
        .await
        .unwrap();
    assert!(off.changed);
    assert_eq!(
        servable(&fx.pool).await,
        0,
        "a mandated talk is not served on consent"
    );
    assert_eq!(
        count(
            &fx.pool,
            "select count(*) from passage where talk_id = $1",
            t.as_uuid()
        )
        .await,
        1
    );
    assert_eq!(
        gate(&fx.pool, t).await,
        Err(GateError::MandateOff),
        "jobs stop at the gate"
    );
    set_talks_basis(&fx.pool, "mandate", "Owner Fixture")
        .await
        .unwrap();
    assert_eq!(servable(&fx.pool).await, 1);
    assert!(
        !set_talks_basis(&fx.pool, "mandate", "Owner Fixture")
            .await
            .unwrap()
            .changed
    );

    let events: Vec<String> = ledger(&fx.pool, None)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.source_key.is_none() && e.teacher.is_none())
        .map(|e| e.event)
        .collect();
    assert_eq!(
        events,
        [
            "talks_basis_mandate",
            "talks_basis_consent",
            "talks_basis_mandate"
        ],
        "each switch, and only a change, writes an event"
    );

    // A disabled source stops its talks too.
    rights::set_enabled(&fx.pool, SOURCE, false, Some("paused"), ACTOR)
        .await
        .unwrap();
    assert_eq!(servable(&fx.pool).await, 0);
    assert_eq!(gate(&fx.pool, t).await, Err(GateError::MandateOff));
}

#[tokio::test]
async fn consent_answers_outrank_the_mandate_and_unknown_speakers_still_wait() {
    let fx = common::start().await;
    talks_source(&fx.pool).await;
    set_talks_basis(&fx.pool, "mandate", "Owner Fixture")
        .await
        .unwrap();

    // Granted consent clears on consent, not the mandate.
    common::consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    let ada = mandated_talk(&fx.pool, ADA).await;
    let mut conn = fx.pool.acquire().await.unwrap();
    assert_eq!(
        clear_waiting(&mut conn, SOURCE, ACTOR).await.unwrap(),
        vec![ada]
    );
    drop(conn);
    assert_eq!(
        basis_of(&fx.pool, ada).await.as_deref(),
        Some("speaker_consent")
    );

    // A declined speaker is never used, mandate or not: the decline also excluded them.
    common::consent(&fx.pool, BO, ConsentStatus::Declined).await;
    let bo = mandated_talk(&fx.pool, BO).await;
    assert_eq!(gate(&fx.pool, bo).await, Err(GateError::Excluded));

    // No speaker: the mandate does not help.
    let nobody = talk(&fx.pool, None, Some("Someone")).await;
    sqlx::query("update talk set source_key = $2 where id = $1")
        .bind(nobody.as_uuid())
        .bind(SOURCE)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(gate(&fx.pool, nobody).await, Err(GateError::NoSpeaker));

    // A talk outside any mandated source still needs consent.
    let elsewhere = talk(&fx.pool, Some(BO), Some(BO)).await;
    assert_eq!(gate(&fx.pool, elsewhere).await, Err(GateError::Excluded));
}

#[tokio::test]
async fn the_constraint_needs_evidence_for_a_mandate_and_the_gate_refuses_opted_out_teachers() {
    let fx = common::start().await;
    talks_source(&fx.pool).await;
    set_talks_basis(&fx.pool, "mandate", "Owner Fixture")
        .await
        .unwrap();
    let t = mandated_talk(&fx.pool, ADA).await;
    let err = sqlx::query(
        "update talk set state = 'cleared', rights_basis = 'mandate', license_evidence_id = null
         where id = $1",
    )
    .bind(t.as_uuid())
    .execute(&fx.pool)
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("past_gate_requires_consent"),
        "{err}"
    );

    opt_out(&fx.pool, ADA, "asked to leave", ACTOR, true)
        .await
        .unwrap();
    assert_eq!(gate(&fx.pool, t).await, Err(GateError::TeacherOptedOut));

    // A person exclusion alone also stops the gate.
    let bo = mandated_talk(&fx.pool, BO).await;
    let mut conn = fx.pool.acquire().await.unwrap();
    rights::add_exclusion(&mut conn, "person", "bo sample", "fixture", None, ACTOR)
        .await
        .unwrap();
    drop(conn);
    assert_eq!(gate(&fx.pool, bo).await, Err(GateError::Excluded));
}

#[tokio::test]
async fn suspending_hides_a_teacher_at_once_and_deletes_nothing() {
    let fx = common::start().await;
    talks_source(&fx.pool).await;
    set_talks_basis(&fx.pool, "mandate", "Owner Fixture")
        .await
        .unwrap();
    let t = mandated_talk(&fx.pool, ADA).await;
    let mut conn = fx.pool.acquire().await.unwrap();
    clear_waiting(&mut conn, SOURCE, ACTOR).await.unwrap();
    drop(conn);
    make_ready(&fx.pool, t).await;
    assert_eq!(servable(&fx.pool).await, 1);

    let out = opt_out(&fx.pool, ADA, "checking a request", ACTOR, true)
        .await
        .unwrap();
    assert_eq!(out.takedown, None);
    assert!(out.exclusions.is_empty());
    assert_eq!(servable(&fx.pool).await, 0, "gone from serving at once");
    assert_eq!(state(&fx.pool, t).await, "ready", "nothing deleted");
    assert_eq!(
        count(
            &fx.pool,
            "select count(*) from passage where talk_id = $1",
            t.as_uuid()
        )
        .await,
        1
    );
    let events: Vec<String> = ledger(&fx.pool, Some(Subject::Teacher("Ada")))
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert_eq!(events, ["suspended"]);
}

#[tokio::test]
async fn a_full_opt_out_deletes_and_excludes_and_a_later_discovery_clears_nothing() {
    let fx = common::start().await;
    talks_source(&fx.pool).await;
    set_talks_basis(&fx.pool, "mandate", "Owner Fixture")
        .await
        .unwrap();
    let t = mandated_talk(&fx.pool, ADA).await;
    let mut conn = fx.pool.acquire().await.unwrap();
    clear_waiting(&mut conn, SOURCE, ACTOR).await.unwrap();
    drop(conn);
    make_ready(&fx.pool, t).await;

    let out = opt_out(&fx.pool, ADA, "please remove my talks", ACTOR, false)
        .await
        .unwrap();
    assert_eq!(
        out.takedown.as_ref().map(|r| r.talks.clone()),
        Some(vec![t])
    );
    assert_eq!(state(&fx.pool, t).await, "withdrawn");
    for table in ["passage", "segment", "transcript"] {
        assert_eq!(
            count(
                &fx.pool,
                &format!("select count(*) from {table} where talk_id = $1"),
                t.as_uuid()
            )
            .await,
            0,
            "{table}"
        );
    }
    let exclusions: Vec<(String, String)> = sqlx::query_as(
        "select scope, pattern from source_exclusion where lifted_at is null order by scope",
    )
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    assert_eq!(
        exclusions,
        [
            ("content".into(), "h".into()),
            ("person".into(), ADA.into())
        ],
        "the person and the removed transcript's hash"
    );

    // A later discovery of another of her talks clears nothing, even with the mandate on.
    let again = mandated_talk(&fx.pool, ADA).await;
    let mut conn = fx.pool.acquire().await.unwrap();
    assert!(
        clear_waiting(&mut conn, SOURCE, ACTOR)
            .await
            .unwrap()
            .is_empty()
    );
    drop(conn);
    assert_eq!(
        state_reason(&fx.pool, again).await.as_deref(),
        Some("opted_out")
    );
    let events: Vec<String> = ledger(&fx.pool, Some(Subject::Teacher(ADA)))
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert_eq!(
        events,
        [
            "suspended",
            "takedown_completed",
            "exclusion_added",
            "exclusion_added"
        ]
    );
}

#[tokio::test]
async fn a_grant_moves_mandated_talks_to_consent_and_a_withdrawal_takes_them_down() {
    let fx = common::start().await;
    talks_source(&fx.pool).await;
    set_talks_basis(&fx.pool, "mandate", "Owner Fixture")
        .await
        .unwrap();
    let ada = mandated_talk(&fx.pool, ADA).await;
    let bo = mandated_talk(&fx.pool, BO).await;
    let mut conn = fx.pool.acquire().await.unwrap();
    clear_waiting(&mut conn, SOURCE, ACTOR).await.unwrap();
    drop(conn);
    make_ready(&fx.pool, ada).await;
    make_ready(&fx.pool, bo).await;
    let served_basis = |pool: PgPool, t: TalkId| async move {
        sqlx::query_scalar::<_, Option<String>>(
            "select rights_basis::text from servable_passage where talk_id = $1",
        )
        .bind(t.as_uuid())
        .fetch_optional(&pool)
        .await
        .unwrap()
        .flatten()
    };
    assert_eq!(
        served_basis(fx.pool.clone(), ada).await.as_deref(),
        Some("mandate")
    );

    // A notice answered with a yes: the talk is now used with permission.
    rights::request(
        &fx.pool,
        Subject::Teacher(ADA),
        "ada@example.invalid",
        "email",
        &record("notice"),
        ACTOR,
    )
    .await
    .unwrap();
    common::consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    assert_eq!(
        basis_of(&fx.pool, ada).await.as_deref(),
        Some("speaker_consent")
    );
    assert_eq!(
        served_basis(fx.pool.clone(), ada).await.as_deref(),
        Some("speaker_consent")
    );
    let status: String =
        sqlx::query_scalar("select status from permission_request where teacher_id is not null")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(status, "granted");

    // A withdrawal runs the takedown and excludes the person.
    common::consent(&fx.pool, BO, ConsentStatus::Withdrawn).await;
    assert_eq!(state(&fx.pool, bo).await, "withdrawn");
    let person: i64 = sqlx::query_scalar(
        "select count(*) from source_exclusion where scope = 'person' and pattern = $1",
    )
    .bind(BO)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(person, 1);
    let events: Vec<String> = ledger(&fx.pool, Some(Subject::Teacher(BO)))
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert!(
        events.starts_with(&["permission_declined".to_owned()]),
        "{events:?}"
    );
}

#[tokio::test]
async fn a_declined_talks_source_takes_its_talks_down() {
    let fx = common::start().await;
    talks_source(&fx.pool).await;
    set_talks_basis(&fx.pool, "mandate", "Owner Fixture")
        .await
        .unwrap();
    let t = mandated_talk(&fx.pool, ADA).await;
    let mut conn = fx.pool.acquire().await.unwrap();
    clear_waiting(&mut conn, SOURCE, ACTOR).await.unwrap();
    drop(conn);
    make_ready(&fx.pool, t).await;

    let out = rights::decline(&fx.pool, SOURCE, &record("no thank you"), ACTOR)
        .await
        .unwrap();
    assert_eq!(out.talks_taken_down, vec![t]);
    assert_eq!(state(&fx.pool, t).await, "withdrawn");
    assert_eq!(servable(&fx.pool).await, 0);
    let scopes: Vec<String> = sqlx::query_scalar(
        "select scope from source_exclusion where source_key = $1 order by scope",
    )
    .bind(SOURCE)
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    assert_eq!(scopes, ["content", "prefix"]);
}

/// Owner's decision of October 14, 2026: a sangha's written yes for its talks source is
/// permission for those talks. The talk keeps serving on its own basis, and the view names the
/// publisher so the page can say "Used with permission of".
#[tokio::test]
async fn a_grant_on_a_talks_source_names_its_publisher_as_permission() {
    let fx = common::start().await;
    talks_source(&fx.pool).await;
    set_talks_basis(&fx.pool, "mandate", "Owner Fixture")
        .await
        .unwrap();
    let ada = mandated_talk(&fx.pool, ADA).await;
    let mut conn = fx.pool.acquire().await.unwrap();
    clear_waiting(&mut conn, SOURCE, ACTOR).await.unwrap();
    drop(conn);
    make_ready(&fx.pool, ada).await;
    let served = |pool: PgPool| async move {
        sqlx::query_as::<_, (String, Option<String>)>(
            "select rights_basis::text, permission_of from servable_passage where talk_id = $1",
        )
        .bind(ada.as_uuid())
        .fetch_optional(&pool)
        .await
        .unwrap()
    };
    assert_eq!(
        served(fx.pool.clone()).await,
        Some(("mandate".to_owned(), None)),
        "on the mandate alone, no permission"
    );

    rights::grant(
        &fx.pool,
        SOURCE,
        &record("Our board is glad for the zendo's talks to be shared."),
        "Fixture Zendo board",
        chrono::NaiveDate::from_ymd_opt(2026, 10, 13).unwrap(),
        "all of the zendo's talks",
        "Owner Fixture",
    )
    .await
    .unwrap();
    let (basis, permission_of) = served(fx.pool.clone()).await.unwrap();
    assert_eq!(basis, "mandate", "the talk keeps serving on its own basis");
    let publisher: String =
        sqlx::query_scalar("select publisher from source_policy where key = $1")
            .bind(SOURCE)
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(permission_of, Some(publisher));
}
