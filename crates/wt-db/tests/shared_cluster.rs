//! Two databases in one Postgres cluster. Roles are cluster-wide but migrations run per
//! database, so a second database used to end up owned by a fresh `dg_owner` that 0010 could not
//! rename. 0020 repairs it: every database ends up as a database on a fresh cluster does.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use wt_db::test_support::TestDb;

/// The last migration before the repair.
const BEFORE_REPAIR: i64 = 19;

async fn connect_to(url: &wt_core::Secret, name: &str) -> PgPool {
    let options = wt_db::connect_options(url).unwrap().database(name);
    PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap()
}

/// Every table, view, sequence, function and enum this schema defines whose owner is not
/// `wt_owner`. Extension members and sqlx's bookkeeping table keep their owners.
async fn not_owned_by_wt_owner(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar(
        "select format('%s %s owned by %s', kind, name, owner) from (
           select 'relation' as kind, c.oid::regclass::text as name,
                  pg_get_userbyid(c.relowner)::text as owner, 'pg_class'::regclass as cls, c.oid
           from pg_class c join pg_namespace n on n.oid = c.relnamespace
           where n.nspname = 'public' and c.relkind in ('r', 'v', 'm', 'S', 'p')
             and c.relname <> '_sqlx_migrations'
           union all
           select 'function', p.oid::regprocedure::text, pg_get_userbyid(p.proowner)::text,
                  'pg_proc'::regclass, p.oid
           from pg_proc p join pg_namespace n on n.oid = p.pronamespace
           where n.nspname = 'public'
           union all
           select 'type', t.oid::regtype::text, pg_get_userbyid(t.typowner)::text,
                  'pg_type'::regclass, t.oid
           from pg_type t join pg_namespace n on n.oid = t.typnamespace
           where n.nspname = 'public' and t.typtype in ('e', 'd')
         ) o
         where owner <> 'wt_owner'
           and not exists (select 1 from pg_depend d
                           where d.classid = o.cls and d.objid = o.oid and d.deptype = 'e')
         order by 1",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

/// Every object in this database that a `dg_*` role owns or holds a privilege on.
async fn held_by_dg_roles(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar(
        "select format('%s %s (%s)', r.rolname, d.classid::regclass, d.deptype)
         from pg_shdepend d join pg_roles r on r.oid = d.refobjid
         where d.refclassid = 'pg_authid'::regclass and r.rolname like 'dg\\_%'
           and d.dbid in (0, (select oid from pg_database where datname = current_database()))
         order by 1",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

/// Every privilege on an object in this database, as `object grantee privilege`.
async fn privileges(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar(
        "select format('%s %s %s %s', name,
                       case when a.grantee = 0 then 'public' else pg_get_userbyid(a.grantee) end,
                       a.privilege_type, a.is_grantable)
         from (
           select c.oid::regclass::text as name, c.relacl as acl
           from pg_class c join pg_namespace n on n.oid = c.relnamespace
           where n.nspname = 'public'
           union all
           select format('%s.%I', c.oid::regclass, att.attname), att.attacl
           from pg_attribute att join pg_class c on c.oid = att.attrelid
             join pg_namespace n on n.oid = c.relnamespace
           where n.nspname = 'public' and att.attnum > 0
           union all
           select p.oid::regprocedure::text, p.proacl
           from pg_proc p join pg_namespace n on n.oid = p.pronamespace
           where n.nspname = 'public'
           union all
           select 'schema ' || n.nspname, n.nspacl from pg_namespace n where n.nspname = 'public'
           union all
           select t.oid::regtype::text, t.typacl
           from pg_type t join pg_namespace n on n.oid = t.typnamespace
           where n.nspname = 'public'
         ) o, aclexplode(o.acl) a
         order by 1",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

/// Reads the serving views and flags a query as `wt_web`, the role the web service runs as.
async fn web_role_can_serve(pool: &PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("set role wt_web")
        .execute(&mut *conn)
        .await
        .unwrap();
    for query in [
        "select count(*) from servable_passage",
        "select count(*) from servable_citation",
        "select flag_query(gen_random_uuid())",
    ] {
        let result = sqlx::query(query).execute(&mut *conn).await;
        assert!(result.is_ok(), "wt_web: {query}: {result:?}");
    }
    sqlx::query("reset role").execute(&mut *conn).await.unwrap();
}

async fn assert_like_a_fresh_cluster(name: &str, pool: &PgPool, reference: &[String]) {
    let foreign = not_owned_by_wt_owner(pool).await;
    assert!(
        foreign.is_empty(),
        "{name}: not owned by wt_owner: {foreign:?}"
    );
    let held = held_by_dg_roles(pool).await;
    assert!(held.is_empty(), "{name}: dg_* roles still hold: {held:?}");
    assert_eq!(
        privileges(pool).await,
        reference,
        "{name}: privileges differ from the database on the fresh cluster"
    );
    web_role_can_serve(pool).await;
}

#[tokio::test]
async fn a_second_database_on_a_shared_cluster_is_owned_by_wt_owner() {
    let db = TestDb::start()
        .await
        .expect("docker must be running for database tests");
    let first = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&first).await.unwrap();
    let reference = privileges(&first).await;
    assert!(not_owned_by_wt_owner(&first).await.is_empty());

    // All migrations at once, as `wt db migrate` and `cargo sqlx migrate run` apply them.
    sqlx::query("create database wt_second")
        .execute(&first)
        .await
        .unwrap();
    let second = connect_to(db.url(), "wt_second").await;
    wt_db::migrate(&second).await.unwrap();
    assert_like_a_fresh_cluster("wt_second", &second, &reference).await;

    // A database migrated before the repair existed, as observed on October 7, 2026.
    sqlx::query("create database wt_third")
        .execute(&first)
        .await
        .unwrap();
    let third = connect_to(db.url(), "wt_third").await;
    wt_db::MIGRATOR.run_to(BEFORE_REPAIR, &third).await.unwrap();
    let broken = not_owned_by_wt_owner(&third).await;
    assert!(
        broken
            .iter()
            .any(|o| o == "relation servable_passage owned by dg_owner"),
        "the shared-cluster bug did not reproduce: {broken:?}"
    );
    let mut conn = third.acquire().await.unwrap();
    sqlx::query("set role wt_web")
        .execute(&mut *conn)
        .await
        .unwrap();
    let denied = sqlx::query("select count(*) from servable_passage")
        .execute(&mut *conn)
        .await;
    assert!(denied.is_err(), "before the repair wt_web cannot serve");
    sqlx::query("reset role").execute(&mut *conn).await.unwrap();
    drop(conn);

    wt_db::migrate(&third).await.unwrap();
    assert_like_a_fresh_cluster("wt_third", &third, &reference).await;

    // The repair dropped the leftover roles and left the first database alone.
    let dg_roles: Vec<String> =
        sqlx::query_scalar("select rolname::text from pg_roles where rolname like 'dg\\_%'")
            .fetch_all(&first)
            .await
            .unwrap();
    assert!(dg_roles.is_empty(), "leftover roles: {dg_roles:?}");
    assert_like_a_fresh_cluster("wt", &first, &reference).await;
}

/// The applied migration versions, newest last.
async fn applied(pool: &PgPool) -> Vec<i64> {
    sqlx::query_scalar("select version from _sqlx_migrations where success order by version")
        .fetch_all(pool)
        .await
        .unwrap()
}

/// From 0009 on a login inheriting `wt_owner` runs migrations. On a healthy database 0020 is a
/// no-op for it. A broken database grants `wt_owner` nothing, not even the schema, and moving
/// `dg_owner`'s objects needs membership in `dg_owner`, so only the bootstrap user that ran
/// 0008 can repair one: the owner login must fail there without changing anything.
#[tokio::test]
async fn a_login_inheriting_wt_owner_runs_0020_but_only_the_bootstrap_user_repairs() {
    const PASSWORD: &str = "test-password-0123456789abcdef";
    let db = TestDb::start()
        .await
        .expect("docker must be running for database tests");

    // Both databases as far as they went before the repair. The second one is broken and
    // brings the dg_* roles back into the cluster.
    let first = wt_db::connect(db.url()).await.unwrap();
    wt_db::MIGRATOR.run_to(BEFORE_REPAIR, &first).await.unwrap();
    sqlx::query("create database wt_second")
        .execute(&first)
        .await
        .unwrap();
    let second = connect_to(db.url(), "wt_second").await;
    wt_db::MIGRATOR
        .run_to(BEFORE_REPAIR, &second)
        .await
        .unwrap();
    let broken = not_owned_by_wt_owner(&second).await;
    assert!(
        !broken.is_empty(),
        "the shared-cluster bug did not reproduce"
    );

    wt_db::create_login(
        &first,
        "wt_owner",
        "wt_owner_login",
        &wt_core::Secret::new(PASSWORD),
    )
    .await
    .unwrap();
    let rest = db.url().expose().split_once('@').unwrap().1;
    let owner_url = wt_core::Secret::new(format!("postgres://wt_owner_login:{PASSWORD}@{rest}"));

    // The healthy database: the owner login applies 0020, which changes nothing there and keeps
    // the dg_* roles the broken database still depends on.
    let owner_first = connect_to(&owner_url, "wt").await;
    let current: String = sqlx::query_scalar("select current_user::text")
        .fetch_one(&owner_first)
        .await
        .unwrap();
    assert_eq!(current, "wt_owner");
    let before = privileges(&first).await;
    assert_eq!(wt_db::migrate(&owner_first).await.unwrap().applied, 1);
    assert_eq!(privileges(&first).await, before);
    assert!(not_owned_by_wt_owner(&first).await.is_empty());
    web_role_can_serve(&first).await;
    let dg_roles: i64 =
        sqlx::query_scalar("select count(*) from pg_roles where rolname like 'dg\\_%'")
            .fetch_one(&first)
            .await
            .unwrap();
    assert_eq!(dg_roles, 4, "the broken database still needs its roles");

    // The broken database: the owner login is refused and nothing changes.
    let owner_second = connect_to(&owner_url, "wt_second").await;
    let refused = wt_db::migrate(&owner_second).await;
    assert!(refused.is_err(), "wt_owner cannot repair: {refused:?}");
    assert_eq!(applied(&second).await.last(), Some(&BEFORE_REPAIR));
    assert_eq!(not_owned_by_wt_owner(&second).await, broken);

    // The bootstrap user repairs it and the leftover roles go.
    wt_db::migrate(&second).await.unwrap();
    assert_like_a_fresh_cluster("wt_second", &second, &before).await;
    let dg_roles: i64 =
        sqlx::query_scalar("select count(*) from pg_roles where rolname like 'dg\\_%'")
            .fetch_one(&first)
            .await
            .unwrap();
    assert_eq!(dg_roles, 0);
}
