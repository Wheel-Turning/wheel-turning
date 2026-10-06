//! The hand list of text locators, for citation resolution (Spec 04, Extraction and citation
//! resolution): `work_locator` rows with their labels and aliases.

use sqlx::PgConnection;
use sqlx::types::Uuid;

use crate::DbError;

/// The hand-listed locator most similar to `text`, by `pg_trgm` similarity against its label and
/// aliases, ignoring case, with that similarity.
pub async fn most_similar(
    conn: &mut PgConnection,
    text: &str,
) -> Result<Option<(Uuid, f32)>, DbError> {
    let row = sqlx::query!(
        r#"select wl.id,
                  greatest(similarity(lower(wl.label), lower($1)),
                           coalesce((select max(similarity(lower(a), lower($1)))
                                     from unnest(wl.aliases) a), 0)) as "sim!"
           from work_locator wl
           order by 2 desc, wl.locator
           limit 1"#,
        text
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|r| (r.id, r.sim)))
}

/// A locator whose label or an alias equals `text` once both are normalized: lowercase,
/// punctuation as spaces, spaces collapsed. `text` comes normalized.
pub async fn exact(conn: &mut PgConnection, text: &str) -> Result<Option<Uuid>, DbError> {
    Ok(sqlx::query_scalar!(
        "select id from work_locator
         where btrim(regexp_replace(lower(label), '[^[:alnum:]]+', ' ', 'g')) = $1
            or exists (select 1 from unnest(aliases) a
                       where btrim(regexp_replace(lower(a), '[^[:alnum:]]+', ' ', 'g')) = $1)
         order by locator limit 1",
        text
    )
    .fetch_optional(&mut *conn)
    .await?)
}

/// The locator with this canonical key, such as `genjokoan:5`.
pub async fn by_key(conn: &mut PgConnection, locator: &str) -> Result<Option<Uuid>, DbError> {
    Ok(sqlx::query_scalar!(
        "select id from work_locator where locator = $1 order by id limit 1",
        locator
    )
    .fetch_optional(&mut *conn)
    .await?)
}
