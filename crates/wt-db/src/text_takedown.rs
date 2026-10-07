//! Taking texts down (Spec 08, Mandated sources and Rights lifecycle): an edition, every
//! edition of a source, or every edition under a URL prefix. Units and embeddings go, the
//! edition stays as a `withdrawn` stub with its hashes so the same document is not brought in
//! again, exclusions cover the work, the edition and every content hash, and `takedown_log`
//! records it. The caller deletes the stored files.

use sqlx::PgConnection;
use sqlx::types::Uuid;

use crate::DbError;
use crate::rights::{self, RightsError};

/// What to take down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextScope {
    /// One edition.
    Edition(Uuid),
    /// Every edition of a source.
    Source(String),
    /// Every edition whose source URL starts with the prefix.
    UrlPrefix(String),
}

/// An edition in scope.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EditionRef {
    /// Its id.
    pub id: Uuid,
    /// Its work's title.
    pub work_title: String,
    /// Its label.
    pub edition_label: String,
    /// Where it came from.
    pub source_url: Option<String>,
    /// Its source, if mandated or licensed through one.
    pub source_key: Option<String>,
}

/// The editions a scope covers, withdrawn ones included so a repeated takedown finishes.
pub async fn editions(
    conn: &mut PgConnection,
    scope: &TextScope,
) -> Result<Vec<EditionRef>, DbError> {
    let (id, key, prefix) = match scope {
        TextScope::Edition(id) => (Some(*id), None, None),
        TextScope::Source(key) => (None, Some(key.as_str()), None),
        TextScope::UrlPrefix(p) => (None, None, Some(p.as_str())),
    };
    Ok(sqlx::query_as!(
        EditionRef,
        "select e.id, w.title as work_title, e.edition_label, e.source_url, e.source_key
         from text_edition e join work w on w.id = e.work_id
         where ($1::uuid is null or e.id = $1)
           and ($2::text is null or e.source_key = $2)
           and ($3::text is null or starts_with(e.source_url, $3))
           and ($1::uuid is not null or $2::text is not null or $3::text is not null)
         order by w.title, e.edition_label",
        id,
        key,
        prefix
    )
    .fetch_all(&mut *conn)
    .await?)
}

/// Units an edition holds.
pub async fn unit_count(conn: &mut PgConnection, edition: Uuid) -> Result<i64, DbError> {
    Ok(sqlx::query_scalar!(
        r#"select count(*) as "n!" from text_unit where edition_id = $1"#,
        edition
    )
    .fetch_one(&mut *conn)
    .await?)
}

/// Text takedown failures.
#[derive(Debug, thiserror::Error)]
pub enum TextTakedownError {
    /// The database refused.
    #[error(transparent)]
    Db(#[from] DbError),
    /// An exclusion could not be written.
    #[error(transparent)]
    Rights(#[from] RightsError),
}

impl From<sqlx::Error> for TextTakedownError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(DbError::Sqlx(e))
    }
}

/// What one edition's removal did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Removed {
    /// The edition.
    pub edition: Uuid,
    /// Units and embeddings deleted.
    pub units: i64,
    /// Citations that pointed at them, now unlinked.
    pub citations_unlinked: i64,
    /// Exclusions added.
    pub exclusions: Vec<Uuid>,
}

/// Removes one edition's text inside the caller's transaction: units deleted, the edition left
/// as a `withdrawn` stub without its legal text, exclusions for the work, the edition and every
/// content hash (its units' and `file_hashes`), and a `takedown_log` row.
pub async fn remove(
    conn: &mut PgConnection,
    edition: &EditionRef,
    file_hashes: &[String],
    reason: &str,
    actor: &str,
) -> Result<Removed, TextTakedownError> {
    let unit_hashes: Vec<String> = sqlx::query_scalar!(
        "select content_hash from text_unit where edition_id = $1 order by ordinal",
        edition.id
    )
    .fetch_all(&mut *conn)
    .await?;
    let citations_unlinked = i64::try_from(
        sqlx::query!(
            "update citation set text_unit_id = null
             where text_unit_id in (select id from text_unit where edition_id = $1)",
            edition.id
        )
        .execute(&mut *conn)
        .await?
        .rows_affected(),
    )
    .unwrap_or(i64::MAX);
    let units = i64::try_from(
        sqlx::query!("delete from text_unit where edition_id = $1", edition.id)
            .execute(&mut *conn)
            .await?
            .rows_affected(),
    )
    .unwrap_or(i64::MAX);
    sqlx::query!(
        "update text_edition set state = 'withdrawn', legal_text = null, legal_pages = '[]',
                                 units_hash = null
         where id = $1",
        edition.id
    )
    .execute(&mut *conn)
    .await?;
    let key = edition.source_key.as_deref();
    let active = rights::active_exclusions(conn).await?;
    let has = |scope: &str, pattern: &str| {
        active
            .iter()
            .any(|x| x.scope == scope && x.pattern.eq_ignore_ascii_case(pattern))
    };
    let mut wanted: Vec<(&str, String)> = vec![
        ("work", edition.work_title.clone()),
        ("edition", edition.id.to_string()),
    ];
    let mut hashes: Vec<String> = unit_hashes
        .into_iter()
        .chain(file_hashes.iter().cloned())
        .collect();
    hashes.sort();
    hashes.dedup();
    wanted.extend(hashes.into_iter().map(|h| ("content", h)));
    let mut exclusions = Vec::new();
    for (scope, pattern) in wanted {
        if !has(scope, &pattern) {
            exclusions
                .push(rights::add_exclusion(conn, scope, &pattern, reason, key, actor).await?);
        }
    }
    let counts = serde_json::json!({
        "text_unit": units,
        "citations_unlinked": citations_unlinked,
        "exclusions": exclusions.len(),
        "reason": reason,
    });
    sqlx::query!(
        "insert into takedown_log (scope, edition_id, source_key, actor, counts)
         values ('edition', $1, $2, $3, $4)",
        edition.id,
        key,
        actor,
        counts
    )
    .execute(&mut *conn)
    .await?;
    Ok(Removed {
        edition: edition.id,
        units,
        citations_unlinked,
        exclusions,
    })
}

/// Adds a prefix exclusion for each of a source's prefixes and logs the source's takedown.
pub async fn close_source(
    conn: &mut PgConnection,
    key: &str,
    reason: &str,
    actor: &str,
) -> Result<Vec<Uuid>, TextTakedownError> {
    let prefixes: Vec<String> = sqlx::query_scalar!(
        "select unnest(url_prefixes) as \"p!\" from source_policy where key = $1",
        key
    )
    .fetch_all(&mut *conn)
    .await?;
    let active = rights::active_exclusions(conn).await?;
    let mut added = Vec::new();
    for p in prefixes {
        if rights::exclusion_for_prefix(&active, &p).is_none() {
            added.push(rights::add_exclusion(conn, "prefix", &p, reason, Some(key), actor).await?);
        }
    }
    sqlx::query!(
        "update source_policy set enabled = false, disabled_reason = $2, disabled_at = now(),
                                  updated_at = now()
         where key = $1",
        key,
        reason
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "insert into takedown_log (scope, source_key, actor, counts) values ('source', $1, $2, $3)",
        key,
        actor,
        serde_json::json!({ "exclusions": added.len(), "reason": reason })
    )
    .execute(&mut *conn)
    .await?;
    Ok(added)
}

/// Sets an edition's state with a reason in `correction_log`: quarantine and release.
pub async fn set_state_logged(
    conn: &mut PgConnection,
    edition: Uuid,
    state: &str,
    reason: &str,
    actor: &str,
) -> Result<Option<String>, DbError> {
    let Some(old) = sqlx::query_scalar!(
        r#"select state::text as "s!" from text_edition where id = $1 for update"#,
        edition
    )
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(None);
    };
    sqlx::query!(
        "update text_edition set state = $2::text::text_state where id = $1",
        edition,
        state
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "insert into correction_log (entity, entity_id, field, old_value, new_value, actor, reason)
         values ('text_edition', $1, 'state', to_jsonb($2::text), to_jsonb($3::text), $4, $5)",
        edition,
        old,
        state,
        actor,
        reason
    )
    .execute(&mut *conn)
    .await?;
    Ok(Some(old))
}

/// Whether the edition's current units have a passing proofread.
pub async fn proofread_passed(conn: &mut PgConnection, edition: Uuid) -> Result<bool, DbError> {
    Ok(sqlx::query_scalar!(
        r#"select exists (select 1 from proof_check pc join text_edition e on e.id = pc.edition_id
                          where e.id = $1 and pc.passed and pc.units_hash = e.units_hash) as "x!""#,
        edition
    )
    .fetch_one(&mut *conn)
    .await?)
}

/// A removal request from the rights page.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Request {
    /// Its id.
    pub id: Uuid,
    /// When it arrived.
    pub received_at: chrono::DateTime<chrono::Utc>,
    /// Who sent it, if they said.
    pub requester_name: Option<String>,
    /// Their address, if given.
    pub requester_email: Option<String>,
    /// Their organization.
    pub organization: Option<String>,
    /// What they named: a URL, a title or an excerpt.
    pub target: String,
    /// Their message.
    pub message: Option<String>,
    /// The excerpt, when they reported one.
    pub text_unit_id: Option<Uuid>,
    /// `open`, `suspended`, `removed` or `declined`.
    pub status: String,
    /// Who handled it.
    pub handled_by: Option<String>,
    /// What was done.
    pub action_taken: Option<String>,
}

/// Requests, open and suspended ones unless `all`.
pub async fn requests(conn: &mut PgConnection, all: bool) -> Result<Vec<Request>, DbError> {
    Ok(sqlx::query_as!(
        Request,
        "select id, received_at, requester_name, requester_email, organization, target, message,
                text_unit_id, status, handled_by, action_taken
         from takedown_request
         where $1 or status in ('open', 'suspended')
         order by received_at",
        all
    )
    .fetch_all(&mut *conn)
    .await?)
}

/// One request.
pub async fn request(conn: &mut PgConnection, id: Uuid) -> Result<Option<Request>, DbError> {
    Ok(sqlx::query_as!(
        Request,
        "select id, received_at, requester_name, requester_email, organization, target, message,
                text_unit_id, status, handled_by, action_taken
         from takedown_request where id = $1",
        id
    )
    .fetch_optional(&mut *conn)
    .await?)
}

/// The edition a request's reported excerpt belongs to.
pub async fn edition_of_unit(conn: &mut PgConnection, unit: Uuid) -> Result<Option<Uuid>, DbError> {
    Ok(
        sqlx::query_scalar!("select edition_id from text_unit where id = $1", unit)
            .fetch_optional(&mut *conn)
            .await?,
    )
}

/// Records how a request was handled.
pub async fn handle_request(
    conn: &mut PgConnection,
    id: Uuid,
    status: &str,
    action: &str,
    actor: &str,
) -> Result<bool, DbError> {
    Ok(sqlx::query!(
        "update takedown_request set status = $2, action_taken = $3, handled_by = $4,
                                     handled_at = now()
         where id = $1",
        id,
        status,
        action,
        actor
    )
    .execute(&mut *conn)
    .await?
    .rows_affected()
        == 1)
}

/// The source's file retention, for an edition with a source.
pub async fn retention(conn: &mut PgConnection, edition: Uuid) -> Result<Option<String>, DbError> {
    Ok(sqlx::query_scalar!(
        "select sp.source_retention from text_edition e join source_policy sp on sp.key = e.source_key
         where e.id = $1",
        edition
    )
    .fetch_optional(&mut *conn)
    .await?)
}

/// Records that an edition's raw files were deleted.
pub async fn mark_source_deleted(conn: &mut PgConnection, edition: Uuid) -> Result<(), DbError> {
    sqlx::query!(
        "update text_edition set source_deleted_at = now() where id = $1",
        edition
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The state of a stored edition with this work and label, if any.
pub async fn existing_state(
    conn: &mut PgConnection,
    title: &str,
    label: &str,
) -> Result<Option<String>, DbError> {
    Ok(sqlx::query_scalar!(
        r#"select e.state::text as "s!" from text_edition e join work w on w.id = e.work_id
           where w.title = $1 and e.edition_label = $2"#,
        title,
        label
    )
    .fetch_optional(&mut *conn)
    .await?)
}

/// A source's `notice_action`: `flag`, `hold` or `block`.
pub async fn notice_action(conn: &mut PgConnection, key: &str) -> Result<Option<String>, DbError> {
    Ok(sqlx::query_scalar!(
        "select notice_action from source_policy where key = $1",
        key
    )
    .fetch_optional(&mut *conn)
    .await?)
}

/// An edition `wt source report` lists: one with a notice, or quarantined.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Noticed {
    /// The edition.
    pub id: Uuid,
    /// Its work.
    pub work_title: String,
    /// Its label.
    pub edition_label: String,
    /// Its source.
    pub source_key: Option<String>,
    /// Its state.
    pub state: String,
    /// The notice text.
    pub notice: Option<String>,
}

/// Editions with a notice or in quarantine, of one source or all.
pub async fn noticed(conn: &mut PgConnection, key: Option<&str>) -> Result<Vec<Noticed>, DbError> {
    Ok(sqlx::query_as!(
        Noticed,
        r#"select e.id, w.title as work_title, e.edition_label, e.source_key,
                  e.state::text as "state!", e.notice
           from text_edition e join work w on w.id = e.work_id
           where (e.notice is not null or e.state = 'quarantined')
             and ($1::text is null or e.source_key = $1)
           order by e.source_key, w.title"#,
        key
    )
    .fetch_all(&mut *conn)
    .await?)
}

/// Whether a source exists and is enabled.
pub async fn source_enabled(conn: &mut PgConnection, key: &str) -> Result<Option<bool>, DbError> {
    Ok(
        sqlx::query_scalar!("select enabled from source_policy where key = $1", key)
            .fetch_optional(&mut *conn)
            .await?,
    )
}
