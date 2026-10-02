//! `dg db create-login`: login roles inherit one group role, run as it, and can be recreated.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use dg_core::Secret;
use dg_db::test_support::TestDb;
use dg_db::{CreateLoginError, LoginOutcome};

const PASSWORD: &str = "test-password-0123456789abcdef";

fn login_url(admin: &Secret, login: &str, password: &str) -> Secret {
    // Swap the superuser credentials in the test database URL for the login's.
    let rest = admin.expose().split_once('@').unwrap().1;
    Secret::new(format!("postgres://{login}:{password}@{rest}"))
}

#[tokio::test]
async fn web_login_runs_as_dg_web_and_sees_only_what_dg_web_may() {
    let db = TestDb::start()
        .await
        .expect("docker must be running for database tests");
    let admin = dg_db::connect(db.url()).await.unwrap();
    dg_db::migrate(&admin).await.unwrap();

    let outcome = dg_db::create_login(&admin, "dg_web", "dg_web_login", &Secret::new(PASSWORD))
        .await
        .unwrap();
    assert_eq!(outcome, LoginOutcome::Created);

    let web = dg_db::connect(&login_url(db.url(), "dg_web_login", PASSWORD))
        .await
        .unwrap();
    let current: String = sqlx::query_scalar("select current_user::text")
        .fetch_one(&web)
        .await
        .unwrap();
    assert_eq!(current, "dg_web", "sessions start as the group role");

    sqlx::query("select count(*) from servable_passage")
        .execute(&web)
        .await
        .expect("dg_web may read the serving view");
    let denied = sqlx::query("select count(*) from passage")
        .execute(&web)
        .await;
    assert!(denied.is_err(), "dg_web must not read the passage table");
}

#[tokio::test]
async fn owner_login_can_run_later_migrations_and_owns_what_it_creates() {
    let db = TestDb::start().await.unwrap();
    let admin = dg_db::connect(db.url()).await.unwrap();
    dg_db::migrate(&admin).await.unwrap();
    dg_db::create_login(&admin, "dg_owner", "dg_owner_login", &Secret::new(PASSWORD))
        .await
        .unwrap();

    let owner = dg_db::connect(&login_url(db.url(), "dg_owner_login", PASSWORD))
        .await
        .unwrap();
    // Up to date, so this only proves the owner can take the migration lock and read history.
    assert_eq!(dg_db::migrate(&owner).await.unwrap().applied, 0);

    sqlx::query("create table owner_probe (id int)")
        .execute(&owner)
        .await
        .expect("dg_owner may create objects in the schema");
    let table_owner: String = sqlx::query_scalar(
        "select pg_get_userbyid(relowner)::text from pg_class where relname = 'owner_probe'",
    )
    .fetch_one(&owner)
    .await
    .unwrap();
    assert_eq!(table_owner, "dg_owner");
}

#[tokio::test]
async fn create_login_is_idempotent_and_validates_input() {
    let db = TestDb::start().await.unwrap();
    let admin = dg_db::connect(db.url()).await.unwrap();
    dg_db::migrate(&admin).await.unwrap();

    let first = dg_db::create_login(
        &admin,
        "dg_pipeline",
        "dg_pipeline_login",
        &Secret::new(PASSWORD),
    )
    .await
    .unwrap();
    let second = dg_db::create_login(
        &admin,
        "dg_pipeline",
        "dg_pipeline_login",
        &Secret::new(PASSWORD),
    )
    .await
    .unwrap();
    assert_eq!(
        (first, second),
        (LoginOutcome::Created, LoginOutcome::Updated)
    );

    assert!(matches!(
        dg_db::create_login(&admin, "postgres", "x_login", &Secret::new(PASSWORD)).await,
        Err(CreateLoginError::UnknownGroup(_))
    ));
    assert!(matches!(
        dg_db::create_login(
            &admin,
            "dg_web",
            "Robert'); drop table talk;--",
            &Secret::new(PASSWORD)
        )
        .await,
        Err(CreateLoginError::InvalidLogin(_))
    ));
    assert!(matches!(
        dg_db::create_login(&admin, "dg_web", "dg_web_login", &Secret::new("short")).await,
        Err(CreateLoginError::WeakPassword)
    ));
}
