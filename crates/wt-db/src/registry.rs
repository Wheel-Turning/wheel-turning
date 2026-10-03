//! Registry import and speaker assignment.

use wt_core::ids::{SanghaId, TalkId, TeacherId};
use wt_registry::{AliasMatcher, RegistryFile, SpeakerMatch};
use sqlx::{PgConnection, PgPool};

use crate::DbError;
use crate::consent::try_clear;
use crate::correction::log_change;

/// What an import changed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ImportReport {
    /// Sanghas created or updated.
    pub sanghas: usize,
    /// Teachers created or updated.
    pub teachers: usize,
    /// Aliases added.
    pub aliases_added: usize,
}

/// Registry errors.
#[derive(Debug, thiserror::Error)]
pub enum RegistryDbError {
    /// The registry file or alias set is invalid.
    #[error(transparent)]
    Registry(#[from] wt_registry::RegistryError),
    /// An alias in the file already belongs to a different teacher in the database.
    #[error("alias {alias:?} already belongs to {owner}")]
    AliasTaken {
        /// The alias.
        alias: String,
        /// Its current owner.
        owner: String,
    },
    /// No teacher matches the name or alias.
    #[error("no teacher is called {0:?}")]
    UnknownTeacher(String),
    /// No sangha has that name.
    #[error("no sangha is called {0:?}")]
    UnknownSangha(String),
    /// The database refused.
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<sqlx::Error> for RegistryDbError {
    fn from(e: sqlx::Error) -> Self {
        RegistryDbError::Db(DbError::Sqlx(e))
    }
}

/// Loads a registry file into the database in one transaction. Idempotent: rerunning updates
/// sangha and teacher fields and adds new aliases. It never removes an alias, since talks may
/// have been matched through it; remove one by hand with a correction_log entry.
pub async fn import(
    pool: &PgPool,
    file: &RegistryFile,
    actor: &str,
) -> Result<ImportReport, RegistryDbError> {
    let mut tx = pool.begin().await?;
    let mut report = ImportReport::default();

    for s in &file.sanghas {
        sqlx::query!(
            r#"insert into sangha (name, lineage, affiliation, donation_url, donation_url_template, listed)
               values ($1, $2, $3, $4, $5, $6)
               on conflict (name) do update set
                 lineage = excluded.lineage, affiliation = excluded.affiliation,
                 donation_url = excluded.donation_url,
                 donation_url_template = excluded.donation_url_template,
                 listed = excluded.listed,
                 donation_verified = case when sangha.donation_url is distinct from excluded.donation_url
                                          then false else sangha.donation_verified end"#,
            s.name,
            s.lineage,
            s.affiliation,
            s.donation_url,
            s.donation_url_template,
            s.listed
        )
        .execute(&mut *tx)
        .await?;
        report.sanghas += 1;
    }

    for t in &file.teachers {
        let teacher_id = sqlx::query_scalar!(
            r#"insert into teacher (display_name, sangha_id)
               values ($1, (select id from sangha where name = $2))
               on conflict (display_name) do update set sangha_id = excluded.sangha_id
               returning id"#,
            t.display_name,
            t.sangha
        )
        .fetch_one(&mut *tx)
        .await?;
        report.teachers += 1;

        for alias in t.all_aliases() {
            let owner = sqlx::query!(
                r#"select te.id, te.display_name from teacher_alias a
                   join teacher te on te.id = a.teacher_id where lower(a.alias) = lower($1)"#,
                alias
            )
            .fetch_optional(&mut *tx)
            .await?;
            match owner {
                Some(o) if o.id != teacher_id => {
                    return Err(RegistryDbError::AliasTaken {
                        alias,
                        owner: o.display_name,
                    });
                }
                Some(_) => {}
                None => {
                    sqlx::query!(
                        "insert into teacher_alias (teacher_id, alias) values ($1, $2)",
                        teacher_id,
                        alias
                    )
                    .execute(&mut *tx)
                    .await?;
                    log_change(
                        &mut tx,
                        "teacher",
                        teacher_id,
                        "alias",
                        None,
                        Some(serde_json::json!(alias)),
                        actor,
                        "registry import",
                    )
                    .await?;
                    report.aliases_added += 1;
                }
            }
        }
    }

    tx.commit().await?;
    Ok(report)
}

/// Finds a teacher by display name or any alias, case-insensitively.
pub async fn find_teacher(
    conn: &mut PgConnection,
    name: &str,
) -> Result<(TeacherId, String), RegistryDbError> {
    let row = sqlx::query!(
        r#"select te.id, te.display_name from teacher te
           where lower(te.display_name) = lower($1)
              or exists (select 1 from teacher_alias a where a.teacher_id = te.id
                                                         and lower(a.alias) = lower($1))"#,
        name
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| RegistryDbError::UnknownTeacher(name.to_owned()))?;
    Ok((TeacherId::from(row.id), row.display_name))
}

/// Finds a sangha by name.
pub async fn find_sangha(conn: &mut PgConnection, name: &str) -> Result<SanghaId, RegistryDbError> {
    sqlx::query_scalar!("select id from sangha where name = $1", name)
        .fetch_optional(&mut *conn)
        .await?
        .map(SanghaId::from)
        .ok_or_else(|| RegistryDbError::UnknownSangha(name.to_owned()))
}

/// Builds the speaker matcher from every alias in the registry.
pub async fn alias_matcher(
    conn: &mut PgConnection,
) -> Result<AliasMatcher<TeacherId>, RegistryDbError> {
    let rows = sqlx::query!("select teacher_id, alias from teacher_alias")
        .fetch_all(&mut *conn)
        .await?;
    Ok(AliasMatcher::new(
        rows.into_iter().map(|r| (TeacherId::from(r.teacher_id), r.alias)),
    )?)
}

/// Where speaker assignment left a talk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeakerOutcome {
    /// Resolved and waiting for consent.
    AwaitingConsent,
    /// Resolved, and consent and rights were already in place, so the talk is cleared.
    Cleared,
    /// Zero or several teachers matched.
    NeedsReview,
}

/// Classification rule 6 (Spec 03, as amended): resolve the speaker from the title and then the
/// description. Exactly one teacher sets `teacher_id`, and `sangha_id` is the sangha where the
/// talk was given (`venue`), which receives its dana. Zero or several matches send the talk to
/// `needs_review` with reason `speaker_unresolved`. Applies only to talks not yet past the gate.
pub async fn assign_speaker(
    conn: &mut PgConnection,
    matcher: &AliasMatcher<TeacherId>,
    talk_id: TalkId,
    venue: SanghaId,
    actor: &str,
) -> Result<SpeakerOutcome, RegistryDbError> {
    let talk = sqlx::query!(
        r#"select t.title, sv.description, t.state::text as "state!"
           from talk t join source_video sv on sv.id = t.source_video_id where t.id = $1"#,
        talk_id.as_uuid()
    )
    .fetch_one(&mut *conn)
    .await?;
    if !matches!(talk.state.as_str(), "discovered" | "needs_review" | "awaiting_consent") {
        return Ok(SpeakerOutcome::NeedsReview);
    }

    match matcher.resolve(&talk.title, talk.description.as_deref()) {
        SpeakerMatch::Resolved { teacher, alias } => {
            sqlx::query!(
                r#"update talk set teacher_id = $2, sangha_id = $3, speaker_guess = $4,
                          state = 'awaiting_consent', state_reason = null,
                          consent_status = coalesce(
                            (select status from current_consent where teacher_id = $2), 'unknown')
                   where id = $1"#,
                talk_id.as_uuid(),
                teacher.as_uuid(),
                venue.as_uuid(),
                alias
            )
            .execute(&mut *conn)
            .await?;
            log_change(
                conn,
                "talk",
                talk_id.as_uuid(),
                "teacher_id",
                None,
                Some(serde_json::json!({ "teacher_id": teacher, "alias": alias })),
                actor,
                "speaker resolved from alias",
            )
            .await?;
            if try_clear(conn, talk_id).await?.is_ok() {
                Ok(SpeakerOutcome::Cleared)
            } else {
                Ok(SpeakerOutcome::AwaitingConsent)
            }
        }
        unresolved => {
            let guess = match &unresolved {
                SpeakerMatch::Ambiguous { aliases } => Some(aliases.join(", ")),
                _ => None,
            };
            sqlx::query!(
                r#"update talk set teacher_id = null, speaker_guess = $2,
                          state = 'needs_review', state_reason = 'speaker_unresolved'
                   where id = $1"#,
                talk_id.as_uuid(),
                guess
            )
            .execute(&mut *conn)
            .await?;
            Ok(SpeakerOutcome::NeedsReview)
        }
    }
}
