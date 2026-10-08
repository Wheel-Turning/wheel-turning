//! Spec 03, classification rule 6: speaker assignment from the alias registry, and the
//! registry import it depends on.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{ACTOR, ADA, REGISTRY, consent, count, state, state_reason};
use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::gate::ConsentStatus;
use wt_core::ids::TalkId;
use wt_db::registry::{
    RegistryDbError, SpeakerOutcome, alias_matcher, assign_speaker, find_sangha, import,
};
use wt_registry::RegistryFile;

/// A freshly discovered talk with no speaker, as discovery leaves it.
async fn discovered(
    pool: &PgPool,
    title: &str,
    description: Option<&str>,
    holder: Option<&str>,
) -> TalkId {
    let video: Uuid = sqlx::query_scalar(
        "insert into source_video (platform, external_id, url, title, description)
         values ('youtube', $1, 'https://example.invalid/v', $2, $3) returning id",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(title)
    .bind(description)
    .fetch_one(pool)
    .await
    .unwrap();
    let id: Uuid = sqlx::query_scalar(
        "insert into talk (source_video_id, title, rights_holder) values ($1, $2, $3) returning id",
    )
    .bind(video)
    .bind(title)
    .bind(holder)
    .fetch_one(pool)
    .await
    .unwrap();
    TalkId::from(id)
}

async fn assign(pool: &PgPool, talk: TalkId) -> SpeakerOutcome {
    let mut conn = pool.acquire().await.unwrap();
    let matcher = alias_matcher(&mut conn).await.unwrap();
    let venue = find_sangha(&mut conn, "Fixture Mountain Zendo")
        .await
        .unwrap();
    assign_speaker(&mut conn, &matcher, talk, venue, ACTOR)
        .await
        .unwrap()
}

async fn teacher_and_guess(pool: &PgPool, talk: TalkId) -> (Option<String>, Option<String>) {
    sqlx::query_as(
        "select te.display_name, t.speaker_guess from talk t
         left join teacher te on te.id = t.teacher_id where t.id = $1",
    )
    .bind(talk.as_uuid())
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn a_title_naming_one_teacher_resolves_and_waits_for_consent() {
    let fx = common::start().await;
    let t = discovered(&fx.pool, "Ada on the precepts", None, None).await;

    assert_eq!(assign(&fx.pool, t).await, SpeakerOutcome::AwaitingConsent);

    assert_eq!(state(&fx.pool, t).await, "awaiting_consent");
    assert_eq!(
        teacher_and_guess(&fx.pool, t).await,
        (Some(ADA.to_owned()), Some("Ada".to_owned()))
    );
    assert_eq!(
        count(
            &fx.pool,
            "select count(*) from correction_log where entity_id = $1 and field = 'teacher_id'",
            t.as_uuid()
        )
        .await,
        1
    );
}

#[tokio::test]
async fn a_resolved_talk_clears_when_consent_and_rights_are_already_in_place() {
    let fx = common::start().await;
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    let t = discovered(&fx.pool, "Ada on the precepts", None, Some(ADA)).await;

    assert_eq!(assign(&fx.pool, t).await, SpeakerOutcome::Cleared);
    assert_eq!(state(&fx.pool, t).await, "cleared");
}

#[tokio::test]
async fn the_description_is_used_when_the_title_names_nobody() {
    let fx = common::start().await;
    let t = discovered(
        &fx.pool,
        "Sunday talk",
        Some("Given by Bo at the zendo"),
        None,
    )
    .await;

    assert_eq!(assign(&fx.pool, t).await, SpeakerOutcome::AwaitingConsent);
    assert_eq!(
        teacher_and_guess(&fx.pool, t).await.0.as_deref(),
        Some("Bo Sample")
    );
}

#[tokio::test]
async fn ambiguous_speakers_land_in_needs_review_with_the_candidates() {
    let fx = common::start().await;
    let t = discovered(&fx.pool, "Ada and Bo in dialogue", None, Some(ADA)).await;

    assert_eq!(assign(&fx.pool, t).await, SpeakerOutcome::NeedsReview);

    assert_eq!(state(&fx.pool, t).await, "needs_review");
    assert_eq!(
        state_reason(&fx.pool, t).await.as_deref(),
        Some("speaker_unresolved")
    );
    let (teacher, guess) = teacher_and_guess(&fx.pool, t).await;
    assert_eq!(
        teacher, None,
        "an ambiguous talk is never credited to anyone"
    );
    let guess = guess.unwrap();
    assert!(guess.contains("Ada") && guess.contains("Bo"), "{guess}");
}

#[tokio::test]
async fn missing_speakers_land_in_needs_review() {
    let fx = common::start().await;
    let t = discovered(
        &fx.pool,
        "Sunday morning talk",
        Some("Recorded at the zendo"),
        None,
    )
    .await;

    assert_eq!(assign(&fx.pool, t).await, SpeakerOutcome::NeedsReview);

    assert_eq!(state(&fx.pool, t).await, "needs_review");
    assert_eq!(
        state_reason(&fx.pool, t).await.as_deref(),
        Some("speaker_unresolved")
    );
    assert_eq!(teacher_and_guess(&fx.pool, t).await, (None, None));
}

#[tokio::test]
async fn a_talk_in_review_resolves_once_its_alias_is_registered() {
    let fx = common::start().await;
    let unknown = discovered(&fx.pool, "Cy Newcomer on zazen", None, None).await;
    assert_eq!(assign(&fx.pool, unknown).await, SpeakerOutcome::NeedsReview);

    let grown = format!(
        "{REGISTRY}
[[teacher]]
display_name = \"Cy Newcomer\"
sangha = \"Fixture Mountain Zendo\"
"
    );
    import(
        &fx.pool,
        &RegistryFile::parse(&grown, "grown").unwrap(),
        ACTOR,
    )
    .await
    .unwrap();

    assert_eq!(
        assign(&fx.pool, unknown).await,
        SpeakerOutcome::AwaitingConsent
    );
    assert_eq!(
        teacher_and_guess(&fx.pool, unknown).await.0.as_deref(),
        Some("Cy Newcomer")
    );
}

#[tokio::test]
async fn talks_past_the_gate_are_never_reassigned() {
    let fx = common::start().await;
    let t = common::talk(&fx.pool, Some(ADA), Some(ADA)).await;
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    sqlx::query("update source_video set title = 'Bo on zazen' where id = (select source_video_id from talk where id = $1)")
        .bind(t.as_uuid())
        .execute(&fx.pool)
        .await
        .unwrap();
    sqlx::query("update talk set title = 'Bo on zazen' where id = $1")
        .bind(t.as_uuid())
        .execute(&fx.pool)
        .await
        .unwrap();

    assign(&fx.pool, t).await;

    assert_eq!(state(&fx.pool, t).await, "cleared");
    assert_eq!(teacher_and_guess(&fx.pool, t).await.0.as_deref(), Some(ADA));
}

#[tokio::test]
async fn import_is_idempotent_and_refuses_to_move_an_alias() {
    let fx = common::start().await;
    let file = RegistryFile::parse(REGISTRY, "again").unwrap();
    let again = import(&fx.pool, &file, ACTOR).await.unwrap();
    assert_eq!(again.aliases_added, 0, "rerunning adds nothing");

    // A later file gives Ada's alias to someone else. The file is valid on its own, so the
    // clash is caught against the database.
    let moved = r#"
[[sangha]]
name = "Fixture Mountain Zendo"

[[teacher]]
display_name = "Ada Impostor"
sangha = "Fixture Mountain Zendo"
aliases = ["ADA"]
"#;
    let file = RegistryFile::parse(moved, "moved").unwrap();
    let err = import(&fx.pool, &file, ACTOR).await.unwrap_err();
    assert!(matches!(err, RegistryDbError::AliasTaken { .. }), "{err:?}");
    let teachers: i64 = sqlx::query_scalar("select count(*) from teacher")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(teachers, 2, "the failed import changed nothing");
}

#[tokio::test]
async fn the_committed_seed_imports_and_is_idempotent() {
    let fx = common::start().await;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/registry.toml");
    let seed = RegistryFile::load(&path).unwrap();

    let first = import(&fx.pool, &seed, ACTOR).await.unwrap();
    assert_eq!(first.teachers, 11);
    let again = import(&fx.pool, &seed, ACTOR).await.unwrap();
    assert_eq!(again.aliases_added, 0);

    let unverified: i64 = sqlx::query_scalar(
        "select count(*) from sangha where not donation_verified and name <> 'Fixture Mountain Zendo'",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(
        unverified, 8,
        "every seeded donation link starts unverified"
    );
}

#[tokio::test]
async fn a_talk_held_in_review_for_another_reason_stays_there() {
    let fx = common::start().await;
    // Spec 03: Q&A sessions with student voices wait in needs_review until a person decides,
    // even when the title names a teacher whose consent is granted.
    consent(&fx.pool, ADA, ConsentStatus::Granted).await;
    let t = discovered(&fx.pool, "Q&A with Ada", None, Some(ADA)).await;
    sqlx::query(
        "update talk set state = 'needs_review', state_reason = 'student_voices' where id = $1",
    )
    .bind(t.as_uuid())
    .execute(&fx.pool)
    .await
    .unwrap();

    assert_eq!(assign(&fx.pool, t).await, SpeakerOutcome::NeedsReview);

    assert_eq!(state(&fx.pool, t).await, "needs_review");
    assert_eq!(
        state_reason(&fx.pool, t).await.as_deref(),
        Some("student_voices")
    );
    assert_eq!(teacher_and_guess(&fx.pool, t).await, (None, None));
}
