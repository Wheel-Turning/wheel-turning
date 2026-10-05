//! Spec 03, the consent gate: `consent set` propagation, `talk set-rights` and `talk exclude`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{
    ACTOR, ADA, BO, consent, count, current_consent, servable, state, state_reason, talk,
};
use wt_core::gate::{ConsentStatus, TalkState};
use wt_db::consent::{
    ConsentError, Holder, RightsTarget, exclude_talk, list_consent, set_consent, set_rights,
};
use wt_db::registry::RegistryDbError;
use wt_db::talk::TalkError;

#[tokio::test]
async fn granting_clears_talks_with_a_known_holder_and_holds_the_rest_as_rights_unknown() {
    let fx = common::start().await;
    let known = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    let unknown_holder = talk(&fx.pool, Some(ADA), None).await;
    let other_teacher = talk(&fx.pool, Some(BO), Some(BO)).await;

    let out = set_consent(
        &fx.pool,
        ADA,
        ConsentStatus::Granted,
        Some("reply-1"),
        None,
        ACTOR,
    )
    .await
    .unwrap();

    assert_eq!(out.cleared, vec![known]);
    assert_eq!(
        out.still_waiting,
        vec![(unknown_holder, "rights_unknown".to_owned())]
    );
    assert_eq!(state(&fx.pool, known).await, "cleared");
    assert_eq!(state(&fx.pool, unknown_holder).await, "awaiting_consent");
    assert_eq!(
        state_reason(&fx.pool, unknown_holder).await.as_deref(),
        Some("rights_unknown")
    );
    assert_eq!(
        state(&fx.pool, other_teacher).await,
        "awaiting_consent",
        "Bo is untouched"
    );

    let basis: String = sqlx::query_scalar("select rights_basis::text from talk where id = $1")
        .bind(known.as_uuid())
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(basis, "speaker_consent");
    assert_eq!(
        count(
            &fx.pool,
            "select count(*) from correction_log
             where entity_id = $1 and field = 'state' and new_value = '\"cleared\"'",
            known.as_uuid()
        )
        .await,
        1,
        "the move to cleared is logged"
    );
}

#[tokio::test]
async fn consent_set_finds_the_teacher_by_alias_in_any_case() {
    let fx = common::start().await;
    let t = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    set_consent(&fx.pool, "aDa", ConsentStatus::Granted, None, None, ACTOR)
        .await
        .unwrap();
    assert_eq!(state(&fx.pool, t).await, "cleared");

    let err = set_consent(
        &fx.pool,
        "Nobody",
        ConsentStatus::Granted,
        None,
        None,
        ACTOR,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            err,
            ConsentError::Registry(RegistryDbError::UnknownTeacher(_))
        ),
        "{err:?}"
    );
}

/// Spec 03, Mandated talks, rule 5: a decline or a withdrawal runs the teacher takedown, so
/// the material is deleted at once, not only held out of serving.
async fn refusal_takes_down_every_talk(status: ConsentStatus) {
    let fx = common::start().await;
    let waiting = talk(&fx.pool, Some(ADA), None).await;
    let served = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    let bo = talk(&fx.pool, Some(BO), Some(BO)).await;
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    consent(&fx.pool, BO, ConsentStatus::Granted).await;
    common::make_ready(&fx.pool, served).await;
    common::make_ready(&fx.pool, bo).await;
    assert_eq!(servable(&fx.pool).await, 2);

    let out = set_consent(&fx.pool, ADA, status, Some("reply-2"), None, ACTOR)
        .await
        .unwrap();

    let report = out.takedown.expect("a refusal runs the takedown");
    let mut taken = report.talks.clone();
    taken.sort();
    let mut expected = vec![waiting, served];
    expected.sort();
    assert_eq!(taken, expected);
    assert_eq!(report.rows.get("passage"), Some(&1));
    assert!(out.cleared.is_empty());

    for t in [waiting, served] {
        assert_eq!(state(&fx.pool, t).await, "withdrawn");
        let talk_status: String =
            sqlx::query_scalar("select consent_status::text from talk where id = $1")
                .bind(t.as_uuid())
                .fetch_one(&fx.pool)
                .await
                .unwrap();
        assert_eq!(talk_status, status.as_str());
        for table in ["passage", "segment", "transcript", "citation", "job"] {
            let sql = format!("select count(*) from {table} where talk_id = $1");
            assert_eq!(count(&fx.pool, &sql, t.as_uuid()).await, 0, "{table}");
        }
    }
    assert_eq!(servable(&fx.pool).await, 1, "only Bo is still served");
    assert_eq!(state(&fx.pool, bo).await, "ready");

    let recorded: Vec<(String, Option<String>)> = sqlx::query_as(
        "select sc.status::text, sc.evidence_ref from speaker_consent sc
         join teacher te on te.id = sc.teacher_id
         where te.display_name = $1 order by sc.created_at",
    )
    .bind(ADA)
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    assert_eq!(
        recorded.last(),
        Some(&(status.as_str().to_owned(), Some("reply-2".to_owned()))),
        "one row for the refusal, with its evidence; the takedown adds no second row"
    );
    assert_eq!(recorded.len(), 2);
    let logged: Vec<String> = sqlx::query_scalar("select scope from takedown_log")
        .fetch_all(&fx.pool)
        .await
        .unwrap();
    assert_eq!(logged, vec!["teacher"]);
}

#[tokio::test]
async fn declining_takes_down_every_talk_of_the_teacher() {
    refusal_takes_down_every_talk(ConsentStatus::Declined).await;
}

#[tokio::test]
async fn withdrawing_takes_down_every_talk_of_the_teacher() {
    refusal_takes_down_every_talk(ConsentStatus::Withdrawn).await;
}

#[tokio::test]
async fn granting_after_a_decline_does_not_bring_talks_back() {
    let fx = common::start().await;
    let t = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    consent(&fx.pool, ADA, ConsentStatus::Declined).await;
    assert_eq!(state(&fx.pool, t).await, "withdrawn");

    let out = set_consent(&fx.pool, ADA, ConsentStatus::Granted, None, None, ACTOR)
        .await
        .unwrap();
    assert!(out.cleared.is_empty());
    assert_eq!(state(&fx.pool, t).await, "withdrawn");
}

#[tokio::test]
async fn a_takedown_that_fails_records_no_refusal() {
    let fx = common::start().await;
    let t = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    let passage = common::make_ready(&fx.pool, t).await;
    // A stored answer the takedown cannot parse makes it fail partway through.
    let answer = common::answer(&fx.pool, &[passage]).await;
    sqlx::query(
        "update answer set payload = jsonb_set(payload, '{themes,0,description}', '5') where id = $1",
    )
    .bind(answer)
    .execute(&fx.pool)
    .await
    .unwrap();

    let err = set_consent(&fx.pool, ADA, ConsentStatus::Declined, None, None, ACTOR)
        .await
        .unwrap_err();

    assert!(matches!(err, ConsentError::Takedown(_)), "{err:?}");
    assert_eq!(
        current_consent(&fx.pool, ADA).await.as_deref(),
        Some("granted"),
        "the consent row and the takedown commit together or not at all"
    );
    assert_eq!(state(&fx.pool, t).await, "ready");
    assert_eq!(servable(&fx.pool).await, 1);
}

#[tokio::test]
async fn a_hand_exclusion_survives_a_later_grant() {
    let fx = common::start().await;
    let t = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    exclude_talk(&fx.pool, t, "speaker asked to hold this one", ACTOR)
        .await
        .unwrap();
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    assert_eq!(state(&fx.pool, t).await, "withdrawn");
    assert_eq!(
        state_reason(&fx.pool, t).await.as_deref(),
        Some("speaker asked to hold this one")
    );
}

#[tokio::test]
async fn consent_falling_back_to_requested_returns_talks_past_the_gate() {
    let fx = common::start().await;
    let cleared = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    let ready = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    common::make_ready(&fx.pool, ready).await;

    let out = set_consent(&fx.pool, ADA, ConsentStatus::Requested, None, None, ACTOR)
        .await
        .unwrap();

    assert_eq!(out.returned_to_awaiting.len(), 2);
    for t in [cleared, ready] {
        assert_eq!(state(&fx.pool, t).await, "awaiting_consent");
        assert_eq!(
            state_reason(&fx.pool, t).await.as_deref(),
            Some("consent_requested")
        );
    }
    assert_eq!(servable(&fx.pool).await, 0);
}

/// A ready talk by Ada cleared by a reviewed, allowed license rather than her consent.
async fn licensed_ready_talk(pool: &sqlx::PgPool) -> wt_core::ids::TalkId {
    let evidence: sqlx::types::Uuid = sqlx::query_scalar(
        "insert into license_evidence (sangha_id, license_code, license_url, evidence_url,
                                       evidence_snapshot_ref, snapshot_sha256, captured_at,
                                       reviewed_by, reviewed_at, allowed)
         select id, 'cc-by-4.0', 'https://example.invalid/l', 'https://example.invalid/e',
                'license-evidence/fixture.txt', 'h', now(), 'reviewer', now(), true
         from sangha where name = 'Fixture Mountain Zendo' returning id",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let t = talk(pool, Some(ADA), Some("Fixture Mountain Zendo")).await;
    sqlx::query(
        "update talk set rights_basis = 'license', license_evidence_id = $2, state = 'cleared'
         where id = $1",
    )
    .bind(t.as_uuid())
    .bind(evidence)
    .execute(pool)
    .await
    .unwrap();
    common::make_ready(pool, t).await;
    t
}

#[tokio::test]
async fn consent_falling_back_leaves_licensed_talks_but_a_decline_takes_them_down() {
    let fx = common::start().await;
    let licensed = licensed_ready_talk(&fx.pool).await;
    let consented = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    common::make_ready(&fx.pool, consented).await;
    assert_eq!(servable(&fx.pool).await, 2);

    let out = set_consent(&fx.pool, ADA, ConsentStatus::Requested, None, None, ACTOR)
        .await
        .unwrap();

    assert_eq!(out.returned_to_awaiting, vec![consented]);
    assert_eq!(state(&fx.pool, consented).await, "awaiting_consent");
    assert_eq!(
        state(&fx.pool, licensed).await,
        "ready",
        "a license does not rest on consent"
    );
    assert_eq!(servable(&fx.pool).await, 1);

    // A speaker who declines is never used, whatever the basis (Spec 03, Mandated talks).
    consent(&fx.pool, ADA, ConsentStatus::Declined).await;
    assert_eq!(state(&fx.pool, licensed).await, "withdrawn");
    assert_eq!(servable(&fx.pool).await, 0);
}

#[tokio::test]
async fn consent_history_is_append_only() {
    let fx = common::start().await;
    consent(&fx.pool, ADA, ConsentStatus::Requested).await;
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    assert_eq!(
        current_consent(&fx.pool, ADA).await.as_deref(),
        Some("granted")
    );

    let rows: i64 = sqlx::query_scalar("select count(*) from speaker_consent")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(rows, 2, "each change appends a row");
    for sql in [
        "update speaker_consent set status = 'declined'",
        "delete from speaker_consent",
        "truncate speaker_consent",
    ] {
        assert!(
            sqlx::query(sql).execute(&fx.pool).await.is_err(),
            "{sql} must fail"
        );
    }
}

#[tokio::test]
async fn a_teacher_with_no_consent_row_never_clears() {
    let fx = common::start().await;
    let t = talk(&fx.pool, Some(ADA), None).await;

    let cleared = set_rights(&fx.pool, RightsTarget::Talk(t), Holder::Speaker, ACTOR)
        .await
        .unwrap();

    assert!(cleared.is_empty());
    assert_eq!(state(&fx.pool, t).await, "awaiting_consent");
    assert_eq!(
        state_reason(&fx.pool, t).await.as_deref(),
        Some("consent_missing")
    );
}

#[tokio::test]
async fn setting_the_speaker_as_holder_clears_a_consented_talk() {
    let fx = common::start().await;
    let t = talk(&fx.pool, Some(ADA), None).await;
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    assert_eq!(state(&fx.pool, t).await, "awaiting_consent");

    let cleared = set_rights(&fx.pool, RightsTarget::Talk(t), Holder::Speaker, ACTOR)
        .await
        .unwrap();

    assert_eq!(cleared, vec![t]);
    assert_eq!(state(&fx.pool, t).await, "cleared");
    let holder: String = sqlx::query_scalar("select rights_holder from talk where id = $1")
        .bind(t.as_uuid())
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(holder, ADA);
    assert_eq!(
        count(
            &fx.pool,
            "select count(*) from correction_log where entity_id = $1 and field = 'rights_holder'",
            t.as_uuid()
        )
        .await,
        1
    );
}

#[tokio::test]
async fn set_rights_by_teacher_covers_all_their_talks() {
    let fx = common::start().await;
    let a = talk(&fx.pool, Some(ADA), None).await;
    let b = talk(&fx.pool, Some(ADA), None).await;
    let bo = talk(&fx.pool, Some(BO), None).await;
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    consent(&fx.pool, BO, ConsentStatus::Granted).await;

    let mut cleared = set_rights(
        &fx.pool,
        RightsTarget::Teacher("Ada".to_owned()),
        Holder::Named("Fixture Mountain Zendo".to_owned()),
        ACTOR,
    )
    .await
    .unwrap();
    cleared.sort();
    let mut expected = vec![a, b];
    expected.sort();

    assert_eq!(cleared, expected);
    assert_eq!(state(&fx.pool, bo).await, "awaiting_consent");
}

#[tokio::test]
async fn a_holder_named_unknown_in_any_case_does_not_clear() {
    let fx = common::start().await;
    let t = talk(&fx.pool, Some(ADA), None).await;
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;

    let cleared = set_rights(
        &fx.pool,
        RightsTarget::Talk(t),
        Holder::Named("  UnKnown ".to_owned()),
        ACTOR,
    )
    .await
    .unwrap();

    assert!(cleared.is_empty());
    assert_eq!(state(&fx.pool, t).await, "awaiting_consent");
    assert_eq!(
        state_reason(&fx.pool, t).await.as_deref(),
        Some("rights_unknown")
    );
}

#[tokio::test]
async fn a_talk_with_no_speaker_cannot_take_the_speaker_as_holder() {
    let fx = common::start().await;
    let t = talk(&fx.pool, None, None).await;
    let err = set_rights(&fx.pool, RightsTarget::Talk(t), Holder::Speaker, ACTOR)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            ConsentError::Registry(RegistryDbError::UnknownTeacher(_))
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn exclude_takes_the_talk_down_and_refuses_missing_or_withdrawn_talks() {
    let fx = common::start().await;
    let t = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    let kept = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    common::make_ready(&fx.pool, t).await;
    common::make_ready(&fx.pool, kept).await;

    let report = exclude_talk(&fx.pool, t, "held out by the speaker", ACTOR)
        .await
        .unwrap();

    assert_eq!(report.talks, vec![t]);
    assert_eq!(report.rows.get("passage"), Some(&1));
    assert_eq!(state(&fx.pool, t).await, "withdrawn");
    assert_eq!(
        state_reason(&fx.pool, t).await.as_deref(),
        Some("held out by the speaker")
    );
    for table in ["passage", "segment", "transcript", "citation", "job"] {
        let sql = format!("select count(*) from {table} where talk_id = $1");
        assert_eq!(count(&fx.pool, &sql, t.as_uuid()).await, 0, "{table}");
    }
    assert_eq!(
        state(&fx.pool, kept).await,
        "ready",
        "the teacher's other talk stays"
    );
    assert_eq!(servable(&fx.pool).await, 1);
    assert_eq!(
        current_consent(&fx.pool, ADA).await.as_deref(),
        Some("granted"),
        "excluding one talk does not change the teacher's consent"
    );
    let logged: Vec<(String, Option<sqlx::types::Uuid>)> =
        sqlx::query_as("select scope, talk_id from takedown_log")
            .fetch_all(&fx.pool)
            .await
            .unwrap();
    assert_eq!(logged, vec![("talk".to_owned(), Some(t.as_uuid()))]);
    assert_eq!(
        count(
            &fx.pool,
            "select count(*) from correction_log
             where entity_id = $1 and field = 'state' and reason = 'held out by the speaker'",
            t.as_uuid()
        )
        .await,
        1
    );

    let missing = wt_core::ids::TalkId::from(sqlx::types::Uuid::new_v4());
    let err = exclude_talk(&fx.pool, missing, "x", ACTOR)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ConsentError::Talk(TalkError::NotFound(id)) if id == missing),
        "{err:?}"
    );

    let gone = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    wt_db::takedown::run(&fx.pool, &wt_db::takedown::Scope::Talk(gone), ACTOR)
        .await
        .unwrap();
    let err = exclude_talk(&fx.pool, gone, "x", ACTOR).await.unwrap_err();
    assert!(
        matches!(
            err,
            ConsentError::Talk(TalkError::WrongState {
                target: TalkState::Withdrawn,
                ..
            })
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn consent_list_shows_latest_status_and_talk_counts() {
    let fx = common::start().await;
    talk(&fx.pool, Some(ADA), Some(ADA)).await;
    talk(&fx.pool, Some(ADA), None).await;
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;

    let rows = list_consent(&fx.pool).await.unwrap();

    assert_eq!(rows.len(), 2);
    let ada = rows.iter().find(|r| r.teacher == ADA).unwrap();
    assert_eq!(ada.status, "granted");
    assert_eq!(ada.evidence_ref.as_deref(), Some("fixture-reply"));
    assert_eq!(ada.talks, "awaiting_consent=1, cleared=1");
    let bo = rows.iter().find(|r| r.teacher == BO).unwrap();
    assert_eq!(bo.status, "unknown");
    assert_eq!(bo.talks, "");
}
