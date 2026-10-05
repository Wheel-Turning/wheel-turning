//! Spec 07 repository guard: every table with a `talk_id` column is registered in the
//! `StoreRegistry`, so takedown and the audit cannot miss it as the schema grows.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use wt_core::stores;
use wt_db::test_support::TestDb;

#[tokio::test]
async fn every_table_with_a_talk_id_is_registered_for_takedown() {
    let db = TestDb::start()
        .await
        .expect("docker must be running for database tests");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();

    let columns: Vec<(String, String)> = sqlx::query_as(
        "select c.table_name::text, c.column_name::text
         from information_schema.columns c
         join information_schema.tables t
           on t.table_schema = c.table_schema and t.table_name = c.table_name
         where c.table_schema = 'public' and t.table_type = 'BASE TABLE'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let columns: BTreeSet<(String, String)> = columns.into_iter().collect();

    let registered: BTreeSet<(String, String)> = stores::tables()
        .map(|(_, table, column)| (table.to_owned(), column.to_owned()))
        .collect();

    let unregistered: Vec<_> = columns
        .iter()
        .filter(|(_, column)| column == "talk_id")
        .filter(|pair| !registered.contains(*pair))
        .map(|(table, column)| format!("{table}.{column}"))
        .collect();
    assert!(
        unregistered.is_empty(),
        "tables with a talk_id column missing from wt_core::stores::STORES: {unregistered:?}"
    );

    let missing: Vec<_> = registered
        .iter()
        .filter(|pair| !columns.contains(*pair))
        .map(|(table, column)| format!("{table}.{column}"))
        .collect();
    assert!(
        missing.is_empty(),
        "registered stores whose table or talk column does not exist: {missing:?}"
    );
}
