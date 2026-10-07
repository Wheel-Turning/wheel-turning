//! Texts (Spec 08): persons, works, editions, units, locators and the import log, written by
//! `wt text import`. Serving reads only `servable_text_unit`.

use sqlx::PgConnection;
use sqlx::types::Uuid;

use crate::DbError;

/// The evidence an import names: whether a person approved it, and its license code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportEvidence {
    /// Approved by a person.
    pub allowed: bool,
    /// Such as `public-domain`.
    pub license_code: String,
}

/// The evidence row, if it exists.
pub async fn evidence(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Option<ImportEvidence>, DbError> {
    let row = sqlx::query!(
        "select allowed, license_code from license_evidence where id = $1",
        id
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|r| ImportEvidence {
        allowed: r.allowed,
        license_code: r.license_code,
    }))
}

/// The person with this display name, created with `kind` if new.
pub async fn upsert_person(
    conn: &mut PgConnection,
    display_name: &str,
    kind: &str,
) -> Result<Uuid, DbError> {
    Ok(sqlx::query_scalar!(
        "insert into person (display_name, kind) values ($1, $2)
         on conflict (display_name) do update set display_name = excluded.display_name
         returning id",
        display_name,
        kind
    )
    .fetch_one(&mut *conn)
    .await?)
}

/// The work with this title, its author and tradition set.
pub async fn upsert_work(
    conn: &mut PgConnection,
    title: &str,
    author: Option<Uuid>,
    tradition: Option<&str>,
) -> Result<Uuid, DbError> {
    Ok(sqlx::query_scalar!(
        "insert into work (title, author_person_id, tradition) values ($1, $2, $3)
         on conflict (title) do update
           set author_person_id = coalesce(excluded.author_person_id, work.author_person_id),
               tradition = coalesce(excluded.tradition, work.tradition)
         returning id",
        title,
        author,
        tradition
    )
    .fetch_one(&mut *conn)
    .await?)
}

/// An edition to write.
#[derive(Debug, Clone)]
pub struct NewEdition<'a> {
    /// Its work.
    pub work: Uuid,
    /// Such as `Muller 1881`.
    pub label: &'a str,
    /// Its translator.
    pub translator: Option<Uuid>,
    /// Language code.
    pub language: &'a str,
    /// Where the source came from.
    pub source_url: Option<&'a str>,
    /// Who holds the rights.
    pub rights_holder: &'a str,
    /// The approved evidence.
    pub evidence: Uuid,
    /// Whether it may be excerpted.
    pub excerptable: bool,
    /// The import key: the source files' and the manifest's hashes.
    pub content_hash: &'a str,
}

/// The stored import key of an edition, if it exists.
pub async fn edition_hash(
    conn: &mut PgConnection,
    title: &str,
    label: &str,
) -> Result<Option<Option<String>>, DbError> {
    Ok(sqlx::query_scalar!(
        "select e.content_hash from text_edition e join work w on w.id = e.work_id
         where w.title = $1 and e.edition_label = $2",
        title,
        label
    )
    .fetch_optional(&mut *conn)
    .await?)
}

/// Writes the edition, ready, and returns its id.
pub async fn upsert_edition(conn: &mut PgConnection, e: &NewEdition<'_>) -> Result<Uuid, DbError> {
    Ok(sqlx::query_scalar!(
        "insert into text_edition (work_id, edition_label, translator_person_id, language,
                                   source_url, rights_holder, license_evidence_id, excerptable,
                                   state, content_hash)
         values ($1, $2, $3, $4, $5, $6, $7, $8, 'ready', $9)
         on conflict (work_id, edition_label) do update
           set translator_person_id = excluded.translator_person_id,
               language = excluded.language, source_url = excluded.source_url,
               rights_holder = excluded.rights_holder,
               license_evidence_id = excluded.license_evidence_id,
               excerptable = excluded.excerptable, state = 'ready',
               content_hash = excluded.content_hash, imported_at = now()
         returning id",
        e.work,
        e.label,
        e.translator,
        e.language,
        e.source_url,
        e.rights_holder,
        e.evidence,
        e.excerptable,
        e.content_hash
    )
    .fetch_one(&mut *conn)
    .await?)
}

/// A unit to store, with its vector.
#[derive(Debug, Clone)]
pub struct NewUnit<'a> {
    /// Its place in the edition.
    pub ordinal: i32,
    /// Its locator.
    pub locator: &'a str,
    /// Its display label.
    pub label: &'a str,
    /// Its section's title.
    pub heading: Option<&'a str>,
    /// The text.
    pub text: &'a str,
    /// Its word count.
    pub word_count: i32,
    /// SHA-256 of the text.
    pub content_hash: String,
    /// The embedding, as pgvector's text form.
    pub embedding: String,
}

/// Replaces the edition's units.
pub async fn replace_units(
    conn: &mut PgConnection,
    edition: Uuid,
    units: &[NewUnit<'_>],
    model: &str,
) -> Result<(), DbError> {
    sqlx::query!("delete from text_unit where edition_id = $1", edition)
        .execute(&mut *conn)
        .await?;
    let ordinals: Vec<i32> = units.iter().map(|u| u.ordinal).collect();
    let locators: Vec<String> = units.iter().map(|u| u.locator.to_owned()).collect();
    let labels: Vec<String> = units.iter().map(|u| u.label.to_owned()).collect();
    let headings: Vec<Option<String>> =
        units.iter().map(|u| u.heading.map(str::to_owned)).collect();
    let texts: Vec<String> = units.iter().map(|u| u.text.to_owned()).collect();
    let counts: Vec<i32> = units.iter().map(|u| u.word_count).collect();
    let hashes: Vec<String> = units.iter().map(|u| u.content_hash.clone()).collect();
    let embeddings: Vec<String> = units.iter().map(|u| u.embedding.clone()).collect();
    sqlx::query!(
        "insert into text_unit (edition_id, ordinal, locator, label, heading, text, word_count,
                                content_hash, embedding, embedding_model)
         select $1, u.ord, u.loc, u.lab, u.head, u.txt, u.wc, u.h, u.emb::vector, $10
         from unnest($2::int4[], $3::text[], $4::text[], $5::text[], $6::text[], $7::int4[],
                     $8::text[], $9::text[]) as u(ord, loc, lab, head, txt, wc, h, emb)",
        edition,
        &ordinals,
        &locators,
        &labels,
        &headings as &[Option<String>],
        &texts,
        &counts,
        &hashes,
        &embeddings,
        model
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Upserts a `work_locator` per unit, with any aliases, so the resolver can match citations.
pub async fn sync_locators(
    conn: &mut PgConnection,
    work: Uuid,
    locators: &[(String, String, Vec<String>)],
) -> Result<usize, DbError> {
    let keys: Vec<String> = locators.iter().map(|(k, _, _)| k.clone()).collect();
    let labels: Vec<String> = locators.iter().map(|(_, l, _)| l.clone()).collect();
    // Postgres arrays cannot be ragged, so aliases travel joined by a unit separator.
    let aliases: Vec<String> = locators.iter().map(|(_, _, a)| a.join("\u{1f}")).collect();
    let n = sqlx::query!(
        r#"insert into work_locator (work_id, locator, label, aliases)
           select $1, u.k, u.l,
                  case when u.a = '' then '{}'::text[] else string_to_array(u.a, chr(31)) end
           from unnest($2::text[], $3::text[], $4::text[]) as u(k, l, a)
           on conflict (work_id, locator) do update
             set label = excluded.label, aliases = excluded.aliases"#,
        work,
        &keys,
        &labels,
        &aliases
    )
    .execute(&mut *conn)
    .await?
    .rows_affected();
    Ok(usize::try_from(n).unwrap_or(usize::MAX))
}

/// Writes the audit row of an importer run.
pub async fn record_run(
    conn: &mut PgConnection,
    manifest_path: &str,
    role: &str,
    dry_run: bool,
    counts: &serde_json::Value,
    actor: &str,
) -> Result<(), DbError> {
    sqlx::query!(
        "insert into import_run (manifest_path, role, dry_run, counts, actor)
         values ($1, $2, $3, $4, $5)",
        manifest_path,
        role,
        dry_run,
        counts,
        actor
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}
