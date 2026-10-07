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
    /// The publisher it names, which the notice scan treats as covered.
    pub publisher: Option<String>,
    /// For a permission, who granted it.
    pub grantor: Option<String>,
}

/// The evidence row, if it exists.
pub async fn evidence(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Option<ImportEvidence>, DbError> {
    let row = sqlx::query!(
        "select allowed, license_code, publisher, grantor from license_evidence where id = $1",
        id
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|r| ImportEvidence {
        allowed: r.allowed,
        license_code: r.license_code,
        publisher: r.publisher,
        grantor: r.grantor,
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
    /// `native`, `text_layer`, `ocr` or `ocr_vision`.
    pub extraction: &'a str,
    /// The OCR engine and version, for an OCR edition.
    pub ocr_engine: Option<&'a str>,
    /// The mean OCR word confidence.
    pub mean_confidence: Option<f32>,
    /// The source it came through, whose `enabled` flag the serving view reads.
    pub source_key: Option<&'a str>,
    /// Its legal pages, kept whole and never embedded or shown.
    pub legal_text: Option<&'a str>,
    /// Where they were found.
    pub legal_pages: serde_json::Value,
    /// Copyright notices naming a holder the evidence does not cover, when flagged or held.
    pub notice: Option<&'a str>,
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

/// Writes the edition and returns its id. It is `imported` until a proofread of its units
/// passes (Spec 08, the proofread gate).
pub async fn upsert_edition(conn: &mut PgConnection, e: &NewEdition<'_>) -> Result<Uuid, DbError> {
    Ok(sqlx::query_scalar!(
        "insert into text_edition (work_id, edition_label, translator_person_id, language,
                                   source_url, rights_holder, license_evidence_id, excerptable,
                                   state, content_hash, extraction, ocr_engine, mean_confidence,
                                   source_key, legal_text, legal_pages, notice)
         values ($1, $2, $3, $4, $5, $6, $7, $8, 'imported', $9, $10, $11, $12, $13, $14, $15,
                 $16)
         on conflict (work_id, edition_label) do update
           set translator_person_id = excluded.translator_person_id,
               language = excluded.language, source_url = excluded.source_url,
               rights_holder = excluded.rights_holder,
               license_evidence_id = excluded.license_evidence_id,
               excerptable = excluded.excerptable, state = 'imported',
               content_hash = excluded.content_hash, extraction = excluded.extraction,
               ocr_engine = excluded.ocr_engine, mean_confidence = excluded.mean_confidence,
               source_key = excluded.source_key, legal_text = excluded.legal_text,
               legal_pages = excluded.legal_pages, notice = excluded.notice, imported_at = now()
         returning id",
        e.work,
        e.label,
        e.translator,
        e.language,
        e.source_url,
        e.rights_holder,
        e.evidence,
        e.excerptable,
        e.content_hash,
        e.extraction,
        e.ocr_engine,
        e.mean_confidence,
        e.source_key,
        e.legal_text,
        e.legal_pages,
        e.notice
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
    /// The PDF page it starts on.
    pub source_page: Option<i32>,
    /// Its mean OCR confidence.
    pub confidence: Option<f32>,
    /// Where it starts in the original: a text fragment for HTML, a page for a PDF.
    pub source_anchor: Option<String>,
}

/// Replaces the edition's units and refreshes its units hash.
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
    let pages: Vec<Option<i32>> = units.iter().map(|u| u.source_page).collect();
    let confidences: Vec<Option<f32>> = units.iter().map(|u| u.confidence).collect();
    let anchors: Vec<Option<String>> = units.iter().map(|u| u.source_anchor.clone()).collect();
    sqlx::query!(
        "insert into text_unit (edition_id, ordinal, locator, label, heading, text, word_count,
                                content_hash, embedding, embedding_model, source_page, confidence,
                                source_anchor)
         select $1, u.ord, u.loc, u.lab, u.head, u.txt, u.wc, u.h, u.emb::vector, $10, u.pg, u.cf,
                u.an
         from unnest($2::int4[], $3::text[], $4::text[], $5::text[], $6::text[], $7::int4[],
                     $8::text[], $9::text[], $11::int4[], $12::float4[], $13::text[])
              as u(ord, loc, lab, head, txt, wc, h, emb, pg, cf, an)",
        edition,
        &ordinals,
        &locators,
        &labels,
        &headings as &[Option<String>],
        &texts,
        &counts,
        &hashes,
        &embeddings,
        model,
        &pages as &[Option<i32>],
        &confidences as &[Option<f32>],
        &anchors as &[Option<String>]
    )
    .execute(&mut *conn)
    .await?;
    refresh_units_hash(conn, edition).await?;
    Ok(())
}

/// Recomputes the hash of an edition's units, which a proofread must match to count.
pub async fn refresh_units_hash(conn: &mut PgConnection, edition: Uuid) -> Result<String, DbError> {
    Ok(sqlx::query_scalar!(
        r#"update text_edition set units_hash = (
             select coalesce(md5(string_agg(content_hash, ',' order by ordinal)), '')
             from text_unit where edition_id = $1)
           where id = $1 returning units_hash as "h!""#,
        edition
    )
    .fetch_one(&mut *conn)
    .await?)
}

/// What the proofread commands need to know of an edition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditionSummary {
    /// Its work's title.
    pub work_title: String,
    /// Its label.
    pub edition_label: String,
    /// How its text was extracted.
    pub extraction: String,
    /// Its units' hash.
    pub units_hash: Option<String>,
    /// Its state.
    pub state: String,
}

/// The edition, if it exists.
pub async fn edition_summary(
    conn: &mut PgConnection,
    edition: Uuid,
) -> Result<Option<EditionSummary>, DbError> {
    let row = sqlx::query!(
        r#"select w.title, e.edition_label, e.extraction, e.units_hash, e.state::text as "state!"
           from text_edition e join work w on w.id = e.work_id where e.id = $1"#,
        edition
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|r| EditionSummary {
        work_title: r.title,
        edition_label: r.edition_label,
        extraction: r.extraction,
        units_hash: r.units_hash,
        state: r.state,
    }))
}

/// A unit as the proofread sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProofUnit {
    /// Its id.
    pub id: Uuid,
    /// Its place.
    pub ordinal: i32,
    /// Its locator.
    pub locator: String,
    /// Its label.
    pub label: String,
    /// Its text.
    pub text: String,
    /// Its OCR confidence.
    pub confidence: Option<f32>,
    /// Its page.
    pub source_page: Option<i32>,
}

/// An edition's units in order.
pub async fn proof_units(
    conn: &mut PgConnection,
    edition: Uuid,
) -> Result<Vec<ProofUnit>, DbError> {
    Ok(sqlx::query_as!(
        ProofUnit,
        "select id, ordinal, locator, label, text, confidence, source_page from text_unit
         where edition_id = $1 order by ordinal",
        edition
    )
    .fetch_all(&mut *conn)
    .await?)
}

/// A proofread to record.
#[derive(Debug, Clone)]
pub struct NewProof<'a> {
    /// The edition.
    pub edition: Uuid,
    /// The units' hash the reviewer read.
    pub units_hash: &'a str,
    /// Who read them.
    pub reviewer: &'a str,
    /// Whether it passed.
    pub passed: bool,
    /// Random units read.
    pub random_sampled: i32,
    /// Errors in them.
    pub random_errors: i32,
    /// Lowest-confidence units read.
    pub worst_sampled: i32,
    /// Errors in them.
    pub worst_errors: i32,
    /// Units read from weak page ranges, counted in the totals.
    pub range_sampled: i32,
    /// Errors in them.
    pub range_errors: i32,
    /// The error notes.
    pub notes: Option<String>,
}

/// Records a proofread and returns its id.
pub async fn record_proof(conn: &mut PgConnection, p: &NewProof<'_>) -> Result<Uuid, DbError> {
    Ok(sqlx::query_scalar!(
        "insert into proof_check (edition_id, sample_size, errors, reviewer, passed, notes,
                                  units_hash, random_sampled, random_errors, worst_sampled,
                                  worst_errors)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) returning id",
        p.edition,
        p.random_sampled + p.worst_sampled + p.range_sampled,
        p.random_errors + p.worst_errors + p.range_errors,
        p.reviewer,
        p.passed,
        p.notes,
        p.units_hash,
        p.random_sampled,
        p.random_errors,
        p.worst_sampled,
        p.worst_errors
    )
    .fetch_one(&mut *conn)
    .await?)
}

/// Sets an edition's state: `imported`, `ready` or `retired`. Returns whether it exists.
pub async fn set_state(
    conn: &mut PgConnection,
    edition: Uuid,
    state: &str,
) -> Result<bool, DbError> {
    Ok(sqlx::query!(
        "update text_edition set state = $2::text::text_state where id = $1",
        edition,
        state
    )
    .execute(&mut *conn)
    .await?
    .rows_affected()
        == 1)
}

/// A correction to one unit.
#[derive(Debug, Clone)]
pub struct UnitFix<'a> {
    /// The unit.
    pub unit: Uuid,
    /// Its new text.
    pub text: &'a str,
    /// Its word count.
    pub word_count: i32,
    /// SHA-256 of the text.
    pub content_hash: String,
    /// Its new embedding, as pgvector's text form.
    pub embedding: String,
    /// The embedding model.
    pub model: &'a str,
    /// Who corrected it.
    pub actor: &'a str,
    /// Why.
    pub reason: &'a str,
}

/// Corrects a unit, writes `correction_log`, refreshes the units hash and returns the edition
/// to `imported`, so it needs a new proofread. Returns the edition and locator, or `None` for
/// an unknown unit.
pub async fn fix_unit(
    conn: &mut PgConnection,
    f: &UnitFix<'_>,
) -> Result<Option<(Uuid, String)>, DbError> {
    let Some(old) = sqlx::query!(
        "select edition_id, locator, text from text_unit where id = $1 for update",
        f.unit
    )
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(None);
    };
    sqlx::query!(
        "update text_unit set text = $2, word_count = $3, content_hash = $4,
                              embedding = $5::text::vector, embedding_model = $6
         where id = $1",
        f.unit,
        f.text,
        f.word_count,
        f.content_hash,
        f.embedding,
        f.model
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "insert into correction_log (entity, entity_id, field, old_value, new_value, actor, reason)
         values ('text_unit', $1, 'text', to_jsonb($2::text), to_jsonb($3::text), $4, $5)",
        f.unit,
        old.text,
        f.text,
        f.actor,
        f.reason
    )
    .execute(&mut *conn)
    .await?;
    refresh_units_hash(conn, old.edition_id).await?;
    set_state(conn, old.edition_id, "imported").await?;
    Ok(Some((old.edition_id, old.locator)))
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

/// What covers a URL for import: approved license or permission evidence listing a prefix of
/// it, or an enabled source whose evidence is approved. A mandate covers only through its
/// source, so switching the source off stops it at once. The longest prefix wins.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Covering {
    /// The evidence.
    pub evidence: Uuid,
    /// `license`, `permission` or `mandate`.
    pub kind: String,
    /// Its code.
    pub license_code: String,
    /// The publisher it names.
    pub publisher: Option<String>,
    /// Who granted a permission.
    pub grantor: Option<String>,
    /// The source, when it comes through one.
    pub source_key: Option<String>,
    /// The prefix that matched.
    pub prefix: String,
}

/// The evidence or source covering `url`, if any.
pub async fn covering_evidence(
    conn: &mut PgConnection,
    url: &str,
) -> Result<Option<Covering>, DbError> {
    Ok(sqlx::query_as!(
        Covering,
        r#"select c.id as "evidence!", c.kind as "kind!", c.license_code as "license_code!",
                  c.publisher, c.grantor, c.source_key, c.prefix as "prefix!"
           from (
             select le.id, le.kind, le.license_code, le.publisher, le.grantor,
                    null::text as source_key, p as prefix
             from license_evidence le, unnest(le.url_prefixes) p
             where le.allowed and le.kind <> 'mandate' and starts_with($1, lower(p))
             union all
             select le.id, le.kind, le.license_code, coalesce(le.publisher, sp.publisher),
                    le.grantor, sp.key, p
             from source_policy sp
             join license_evidence le on le.id = sp.license_evidence_id,
                  unnest(sp.url_prefixes) p
             where sp.enabled and le.allowed and starts_with($1, lower(p))
           ) c
           order by length(c.prefix) desc, c.source_key nulls last
           limit 1"#,
        url
    )
    .fetch_optional(&mut *conn)
    .await?)
}

/// Each unit's locator and content hash.
pub async fn unit_digest(
    conn: &mut PgConnection,
    edition: Uuid,
) -> Result<Vec<(String, String)>, DbError> {
    Ok(sqlx::query!(
        "select locator, content_hash from text_unit where edition_id = $1 order by ordinal",
        edition
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|r| (r.locator, r.content_hash))
    .collect())
}

/// Records what was fetched for an edition.
pub async fn set_fetch_info(
    conn: &mut PgConnection,
    edition: Uuid,
    sha256: &str,
    etag: Option<&str>,
) -> Result<(), DbError> {
    sqlx::query!(
        "update text_edition set source_sha256 = $2, source_etag = $3, fetched_at = now()
         where id = $1",
        edition,
        sha256,
        etag
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
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
