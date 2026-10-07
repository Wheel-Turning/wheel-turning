//! Reference assets (Spec 08, Import pipeline): alias lists, lineage, locator lists and the
//! glossary's provenance, written by `wt reference import`, and the link from citations to
//! text units. Reference assets feed the pipeline and are never shown.

use sqlx::PgConnection;
use sqlx::types::Uuid;

use crate::DbError;

/// Whether an asset with this hash is already recorded.
pub async fn asset_exists(conn: &mut PgConnection, sha256: &str) -> Result<bool, DbError> {
    Ok(sqlx::query_scalar!(
        r#"select exists (select 1 from reference_asset where sha256 = $1) as "e!""#,
        sha256
    )
    .fetch_one(&mut *conn)
    .await?)
}

/// An asset to record.
#[derive(Debug, Clone)]
pub struct NewAsset<'a> {
    /// `glossary`, `alias_list`, `lineage`, `locator_list`, `chant_metadata` or `other`.
    pub kind: &'a str,
    /// Its title.
    pub title: &'a str,
    /// Where it came from.
    pub source_url: Option<&'a str>,
    /// Its evidence, if it is not the founder's own compilation.
    pub evidence: Option<Uuid>,
    /// Where its files are kept.
    pub s3_key: &'a str,
    /// The files' and the manifest's hash.
    pub sha256: &'a str,
    /// Rows written per table.
    pub row_counts: serde_json::Value,
    /// Who imported it.
    pub imported_by: &'a str,
}

/// Records an asset and returns its id.
pub async fn record_asset(conn: &mut PgConnection, a: &NewAsset<'_>) -> Result<Uuid, DbError> {
    Ok(sqlx::query_scalar!(
        "insert into reference_asset (kind, title, source_url, license_evidence_id, s3_key,
                                      sha256, row_counts, imported_by)
         values ($1::text::reference_kind, $2, $3, $4, $5, $6, $7, $8) returning id",
        a.kind,
        a.title,
        a.source_url,
        a.evidence,
        a.s3_key,
        a.sha256,
        a.row_counts,
        a.imported_by
    )
    .fetch_one(&mut *conn)
    .await?)
}

/// Sets the asset's row counts once they are known.
pub async fn set_row_counts(
    conn: &mut PgConnection,
    asset: Uuid,
    counts: &serde_json::Value,
) -> Result<(), DbError> {
    sqlx::query!(
        "update reference_asset set row_counts = $2 where id = $1",
        asset,
        counts
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Marks glossary terms as coming from an asset, where no source is recorded yet.
pub async fn tag_glossary_terms(
    conn: &mut PgConnection,
    asset: Uuid,
    canonical: &[String],
) -> Result<u64, DbError> {
    Ok(sqlx::query!(
        "update glossary_term set source_asset_id = $1
         where canonical = any($2) and source_asset_id is null",
        asset,
        canonical
    )
    .execute(&mut *conn)
    .await?
    .rows_affected())
}

/// Adds an alias to a person. Returns the person who already holds it, when it is someone
/// else's, and adds nothing then.
pub async fn add_person_alias(
    conn: &mut PgConnection,
    person: Uuid,
    alias: &str,
) -> Result<Option<String>, DbError> {
    let holder = sqlx::query!(
        "select p.id, p.display_name from person_alias a join person p on p.id = a.person_id
         where lower(a.alias) = lower($1)",
        alias
    )
    .fetch_optional(&mut *conn)
    .await?;
    match holder {
        Some(h) if h.id == person => Ok(None),
        Some(h) => Ok(Some(h.display_name)),
        None => {
            sqlx::query!(
                "insert into person_alias (person_id, alias) values ($1, $2)",
                person,
                alias
            )
            .execute(&mut *conn)
            .await?;
            Ok(None)
        }
    }
}

/// Adds a lineage edge unless it exists. Returns whether it was new.
pub async fn add_lineage_edge(
    conn: &mut PgConnection,
    parent: Uuid,
    child: Uuid,
    kind: &str,
    asset: Uuid,
    note: Option<&str>,
) -> Result<bool, DbError> {
    Ok(sqlx::query!(
        "insert into lineage_edge (parent_person_id, child_person_id, kind, source_asset_id, note)
         values ($1, $2, $3, $4, $5) on conflict do nothing",
        parent,
        child,
        kind,
        asset,
        note
    )
    .execute(&mut *conn)
    .await?
    .rows_affected()
        == 1)
}

/// Links teachers to the persons whose display names match theirs exactly, where a teacher has
/// no person yet. Returns the teachers linked.
pub async fn link_teachers_exactly(
    conn: &mut PgConnection,
    persons: &[Uuid],
) -> Result<Vec<String>, DbError> {
    Ok(sqlx::query_scalar!(
        "update teacher t set person_id = p.id from person p
         where p.id = any($1) and p.display_name = t.display_name and t.person_id is null
         returning t.display_name",
        persons
    )
    .fetch_all(&mut *conn)
    .await?)
}

/// A hand-listed locator.
#[derive(Debug, Clone)]
pub struct NewLocator<'a> {
    /// Such as `genjokoan:5`.
    pub locator: &'a str,
    /// Such as `Genjokoan, paragraph 5`.
    pub label: &'a str,
    /// Other ways talks refer to it.
    pub aliases: &'a [String],
    /// Where to read it, when it is a link-out.
    pub link_out_url: Option<&'a str>,
}

/// Upserts a work's locators, keeping the asset that listed them.
pub async fn upsert_locators(
    conn: &mut PgConnection,
    work: Uuid,
    locators: &[NewLocator<'_>],
    asset: Option<Uuid>,
) -> Result<usize, DbError> {
    for l in locators {
        sqlx::query!(
            "insert into work_locator (work_id, locator, label, aliases, link_out_url,
                                       source_asset_id)
             values ($1, $2, $3, $4, $5, $6)
             on conflict (work_id, locator) do update
               set label = excluded.label, aliases = excluded.aliases,
                   link_out_url = coalesce(excluded.link_out_url, work_locator.link_out_url),
                   source_asset_id = coalesce(excluded.source_asset_id,
                                              work_locator.source_asset_id)",
            work,
            l.locator,
            l.label,
            l.aliases,
            l.link_out_url,
            asset
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(locators.len())
}

/// Sets a work's link-out address, creating the work if new.
pub async fn upsert_link_out_work(
    conn: &mut PgConnection,
    title: &str,
    url: &str,
) -> Result<Uuid, DbError> {
    Ok(sqlx::query_scalar!(
        "insert into work (title, link_out_url) values ($1, $2)
         on conflict (title) do update set link_out_url = excluded.link_out_url
         returning id",
        title,
        url
    )
    .fetch_one(&mut *conn)
    .await?)
}

/// Sets `citation.text_unit_id` wherever a citation's resolved locator has a unit of the same
/// locator in a ready, excerptable edition of its work, and clears it where that is no longer
/// so. The serving view still shows only proofread units. Returns the rows changed.
pub async fn link_citations(conn: &mut PgConnection) -> Result<u64, DbError> {
    let linked = sqlx::query!(
        "update citation c set text_unit_id = u.id
         from work_locator wl
         join text_edition e on e.work_id = wl.work_id
         join text_unit u on u.edition_id = e.id and u.locator = wl.locator
         where c.work_locator_id = wl.id and e.state = 'ready' and e.excerptable
           and c.text_unit_id is distinct from u.id"
    )
    .execute(&mut *conn)
    .await?
    .rows_affected();
    let cleared = sqlx::query!(
        "update citation c set text_unit_id = null
         where c.text_unit_id is not null and not exists (
           select 1 from work_locator wl
           join text_edition e on e.work_id = wl.work_id
           join text_unit u on u.edition_id = e.id and u.locator = wl.locator
           where wl.id = c.work_locator_id and u.id = c.text_unit_id
             and e.state = 'ready' and e.excerptable)"
    )
    .execute(&mut *conn)
    .await?
    .rows_affected();
    Ok(linked + cleared)
}

/// A hash of the hand list: every locator with its aliases, so resolution reruns when it
/// changes.
pub async fn locator_version(conn: &mut PgConnection) -> Result<String, DbError> {
    Ok(sqlx::query_scalar!(
        r#"select coalesce(md5(string_agg(w.title || chr(31) || wl.locator || chr(31)
                                          || array_to_string(wl.aliases, chr(30)), chr(29)
                                          order by w.title, wl.locator)), '') as "v!"
           from work_locator wl join work w on w.id = wl.work_id"#
    )
    .fetch_one(&mut *conn)
    .await?)
}
