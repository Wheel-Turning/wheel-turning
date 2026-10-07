//! Spec 03, Display levels: each level changes what `servable_passage` returns, a talk can be
//! narrower but never wider than its teacher, `full` is the default, and widening needs the
//! teacher's recorded word. Everything is synthetic.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{ACTOR, ADA, BO, consent, make_ready, talk};
use sqlx::PgPool;
use wt_core::gate::ConsentStatus;
use wt_core::ids::TalkId;
use wt_db::display::{self, DisplayError, DisplayLevel};

/// The talk's effective level in the serving view, or `None` when it is not servable.
async fn served(pool: &PgPool, talk: TalkId) -> Option<String> {
    sqlx::query_scalar("select display_level from servable_passage where talk_id = $1 limit 1")
        .bind(talk.as_uuid())
        .fetch_optional(pool)
        .await
        .unwrap()
}

async fn ready_talk(pool: &PgPool, teacher: &str) -> TalkId {
    let t = talk(pool, Some(teacher), Some(teacher)).await;
    // Recording consent clears the teacher's waiting talks.
    consent(pool, teacher, ConsentStatus::Granted).await;
    make_ready(pool, t).await;
    t
}

async fn set(
    pool: &PgPool,
    teacher: &str,
    talk: Option<TalkId>,
    to: DisplayLevel,
    evidence: Option<&str>,
) -> Result<display::LevelChange, DisplayError> {
    let mut conn = pool.acquire().await.unwrap();
    display::set_level(
        &mut conn,
        teacher,
        talk,
        to,
        ACTOR,
        "fixture request",
        evidence,
    )
    .await
}

#[tokio::test]
async fn each_level_changes_what_the_serving_view_returns() {
    let fx = common::start().await;
    let a = ready_talk(&fx.pool, ADA).await;
    let b = ready_talk(&fx.pool, BO).await;

    // Full is the default.
    assert_eq!(served(&fx.pool, a).await.as_deref(), Some("full"));

    for (to, expected) in [
        (DisplayLevel::Excerpts, Some("excerpts")),
        (DisplayLevel::OwnerOnly, None),
        (DisplayLevel::Excluded, None),
    ] {
        set(&fx.pool, ADA, None, to, None).await.unwrap();
        assert_eq!(served(&fx.pool, a).await.as_deref(), expected, "{to}");
    }
    // Bo is untouched throughout.
    assert_eq!(served(&fx.pool, b).await.as_deref(), Some("full"));

    // Widening needs the teacher's recorded word.
    let err = set(&fx.pool, ADA, None, DisplayLevel::Full, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DisplayError::WideningNeedsEvidence { .. }),
        "{err}"
    );
    let change = set(
        &fx.pool,
        ADA,
        None,
        DisplayLevel::Full,
        Some("reply-2026-10-07"),
    )
    .await
    .unwrap();
    assert_eq!(
        (change.from, change.to, change.changed),
        (DisplayLevel::Excluded, DisplayLevel::Full, true)
    );
    assert_eq!(served(&fx.pool, a).await.as_deref(), Some("full"));

    // Takedown levels have their own commands.
    for to in [DisplayLevel::Removed, DisplayLevel::ExportThenDelete] {
        let err = set(&fx.pool, ADA, None, to, Some("x")).await.unwrap_err();
        assert!(matches!(err, DisplayError::Takedown(_)), "{err}");
    }

    // Every change is logged, with the evidence.
    let logged: Vec<(String, String)> = sqlx::query_as(
        "select new_value #>> '{}', reason from correction_log
         where field = 'display_level' order by created_at, id",
    )
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    let levels: Vec<&str> = logged.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(levels, ["excerpts", "owner_only", "excluded", "full"]);
    assert!(logged[3].1.contains("reply-2026-10-07"));
}

#[tokio::test]
async fn a_talk_can_be_narrower_than_its_teacher_but_never_wider() {
    let fx = common::start().await;
    let one = ready_talk(&fx.pool, ADA).await;
    let two = ready_talk(&fx.pool, ADA).await;

    // One talk narrowed: the other stays full.
    set(&fx.pool, ADA, Some(one), DisplayLevel::Excerpts, None)
        .await
        .unwrap();
    assert_eq!(served(&fx.pool, one).await.as_deref(), Some("excerpts"));
    assert_eq!(served(&fx.pool, two).await.as_deref(), Some("full"));
    set(&fx.pool, ADA, Some(one), DisplayLevel::Excluded, None)
        .await
        .unwrap();
    assert_eq!(served(&fx.pool, one).await, None);

    // The teacher narrows to excerpts: talk two follows, talk one stays excluded.
    set(&fx.pool, ADA, None, DisplayLevel::Excerpts, None)
        .await
        .unwrap();
    assert_eq!(served(&fx.pool, two).await.as_deref(), Some("excerpts"));
    assert_eq!(served(&fx.pool, one).await, None);

    // A talk cannot be set wider than its teacher, nor to full, e or f.
    for to in [DisplayLevel::Full, DisplayLevel::Removed] {
        let err = set(&fx.pool, ADA, Some(two), to, Some("x"))
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                DisplayError::TalkLevel { .. } | DisplayError::Takedown(_)
            ),
            "{err}"
        );
    }
    set(&fx.pool, ADA, None, DisplayLevel::OwnerOnly, None)
        .await
        .unwrap();
    let err = set(&fx.pool, ADA, Some(two), DisplayLevel::Excerpts, Some("x"))
        .await
        .unwrap_err();
    assert!(matches!(err, DisplayError::TalkLevel { .. }), "{err}");

    // A talk of another teacher is refused.
    let bo = talk(&fx.pool, Some(BO), Some(BO)).await;
    let err = set(&fx.pool, ADA, Some(bo), DisplayLevel::Excluded, None)
        .await
        .unwrap_err();
    assert!(matches!(err, DisplayError::NotTheirTalk(_)), "{err}");
}

#[tokio::test]
async fn the_database_refuses_levels_it_does_not_know() {
    let fx = common::start().await;
    let bad = sqlx::query("update teacher set display_level = 'public' where display_name = $1")
        .bind(ADA)
        .execute(&fx.pool)
        .await;
    assert!(bad.is_err());
    let t = talk(&fx.pool, Some(ADA), Some(ADA)).await;
    let wide = sqlx::query("update talk set display_level = 'full' where id = $1")
        .bind(t.as_uuid())
        .execute(&fx.pool)
        .await;
    assert!(
        wide.is_err(),
        "a talk-level full is meaningless and refused"
    );
    let reviewed = sqlx::query(
        "insert into transcript (talk_id, version, origin, s3_key, content_hash)
         values ($1, 9, 'reviewed', 'k', 'h')",
    )
    .bind(t.as_uuid())
    .execute(&fx.pool)
    .await;
    assert!(reviewed.is_ok(), "{reviewed:?}");
}
