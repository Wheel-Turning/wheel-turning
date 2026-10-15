//! Registry import and speaker assignment.

use sqlx::{PgConnection, PgPool};
use wt_core::ids::{SanghaId, TalkId, TeacherId};
use wt_registry::{AliasMatcher, RegistryFile, SpeakerMatch};

use crate::DbError;
use crate::consent::try_clear;
use crate::correction::{Change, log_change};
use crate::talk::TalkError;

/// What an import changed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize)]
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
    /// The talk is held for another reason or past the gate, so its speaker is not open.
    #[error(
        "talk {talk} is {state}; only a talk before the gate, or held as speaker_unresolved, takes a speaker by hand"
    )]
    SpeakerClosed {
        /// The talk.
        talk: TalkId,
        /// Its state, with the reason when it has one.
        state: String,
    },
    /// Clearing a newly assigned talk through the gate failed.
    #[error(transparent)]
    Talk(#[from] TalkError),
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
        // A changed link, template or kind clears the verification: a person checked the old
        // one, not this one (Spec 02, Donation links).
        sqlx::query!(
            r#"insert into sangha (name, lineage, affiliation, donation_url, donation_url_template,
                                   listed, donation_kind, donation_note)
               values ($1, $2, $3, $4, $5, $6, $7, $8)
               on conflict (name) do update set
                 lineage = excluded.lineage, affiliation = excluded.affiliation,
                 donation_url = excluded.donation_url,
                 donation_url_template = excluded.donation_url_template,
                 listed = excluded.listed,
                 donation_kind = excluded.donation_kind,
                 donation_note = excluded.donation_note,
                 donation_verified = case
                   when (sangha.donation_url, sangha.donation_url_template, sangha.donation_kind)
                        is distinct from
                        (excluded.donation_url, excluded.donation_url_template, excluded.donation_kind)
                   then false else sangha.donation_verified end"#,
            s.name,
            s.lineage,
            s.affiliation,
            s.donation_url,
            s.donation_url_template,
            s.listed,
            s.kind(),
            s.donation_note
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
                        &Change {
                            entity: "teacher",
                            entity_id: teacher_id,
                            field: "alias",
                            old_value: None,
                            new_value: Some(serde_json::json!(alias)),
                            actor,
                            reason: "registry import",
                        },
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
        rows.into_iter()
            .map(|r| (TeacherId::from(r.teacher_id), r.alias)),
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
/// `needs_review` with reason `speaker_unresolved`. Applies only to talks not yet past the gate,
/// and leaves alone a talk held in `needs_review` for any other reason, such as student voices
/// or an unclear talk type: only a person releases those. A speaker assigned by hand is kept.
pub async fn assign_speaker(
    conn: &mut PgConnection,
    matcher: &AliasMatcher<TeacherId>,
    talk_id: TalkId,
    venue: SanghaId,
    actor: &str,
) -> Result<SpeakerOutcome, RegistryDbError> {
    let talk = sqlx::query!(
        r#"select t.title, sv.description, t.state::text as "state!", t.state_reason,
                  t.speaker_assigned_by is not null as "by_hand!",
                  sv.platform::text = 'engagewisdom' as "byline_only!"
           from talk t join source_video sv on sv.id = t.source_video_id where t.id = $1"#,
        talk_id.as_uuid()
    )
    .fetch_one(&mut *conn)
    .await?;
    if !speaker_open(&talk.state, talk.state_reason.as_deref()) {
        return Ok(SpeakerOutcome::NeedsReview);
    }
    if talk.by_hand {
        // A person's decision outranks the registry (Spec 03, Assigning a speaker by hand).
        sqlx::query!(
            "update talk set state = 'awaiting_consent', state_reason = null where id = $1",
            talk_id.as_uuid()
        )
        .execute(&mut *conn)
        .await?;
        return clear_if_ready(conn, talk_id).await;
    }

    // A source that names its speaker is read from that line alone, so a title naming another
    // teacher does not make the talk ambiguous (Spec 03, EngageWisdom sources, rule 3).
    let title = if talk.byline_only { "" } else { &talk.title };
    match matcher.resolve(title, talk.description.as_deref()) {
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
                &Change {
                    entity: "talk",
                    entity_id: talk_id.as_uuid(),
                    field: "teacher_id",
                    old_value: None,
                    new_value: Some(serde_json::json!({ "teacher_id": teacher, "alias": alias })),
                    actor,
                    reason: "speaker resolved from alias",
                },
            )
            .await?;
            clear_if_ready(conn, talk_id).await
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

/// Spec 03, Assigning a speaker by hand: sets `teacher` as the speaker of a talk at
/// `discovered`, `awaiting_consent`, or `needs_review` with reason `speaker_unresolved`, with
/// `sangha` as the venue, and records `actor` in `speaker_assigned_by` so classification keeps
/// it. The talk moves to `awaiting_consent` and is cleared at once when its rights basis is in
/// place. Any other talk is refused with [`RegistryDbError::SpeakerClosed`].
pub async fn assign_speaker_by_hand(
    conn: &mut PgConnection,
    talk_id: TalkId,
    teacher: TeacherId,
    sangha: SanghaId,
    actor: &str,
) -> Result<SpeakerOutcome, RegistryDbError> {
    let talk = sqlx::query!(
        r#"select t.state::text as "state!", t.state_reason, t.teacher_id
           from talk t where t.id = $1 for update"#,
        talk_id.as_uuid()
    )
    .fetch_one(&mut *conn)
    .await?;
    if !speaker_open(&talk.state, talk.state_reason.as_deref()) {
        let state = match talk.state_reason {
            Some(reason) => format!("{} ({reason})", talk.state),
            None => talk.state,
        };
        return Err(RegistryDbError::SpeakerClosed {
            talk: talk_id,
            state,
        });
    }
    sqlx::query!(
        r#"update talk set teacher_id = $2, sangha_id = $3, speaker_assigned_by = $4,
                  state = 'awaiting_consent', state_reason = null,
                  consent_status = coalesce(
                    (select status from current_consent where teacher_id = $2), 'unknown')
           where id = $1"#,
        talk_id.as_uuid(),
        teacher.as_uuid(),
        sangha.as_uuid(),
        actor
    )
    .execute(&mut *conn)
    .await?;
    log_change(
        conn,
        &Change {
            entity: "talk",
            entity_id: talk_id.as_uuid(),
            field: "teacher_id",
            old_value: talk
                .teacher_id
                .map(|t| serde_json::json!({ "teacher_id": t })),
            new_value: Some(serde_json::json!({ "teacher_id": teacher, "sangha_id": sangha })),
            actor,
            reason: "speaker assigned by hand",
        },
    )
    .await?;
    clear_if_ready(conn, talk_id).await
}

/// The source a talk was discovered from, whose `default_sangha` is its venue.
pub async fn talk_source_key(
    conn: &mut PgConnection,
    talk_id: TalkId,
) -> Result<Option<String>, RegistryDbError> {
    Ok(sqlx::query_scalar!(
        "select source_key from talk where id = $1",
        talk_id.as_uuid()
    )
    .fetch_optional(&mut *conn)
    .await?
    .flatten())
}

/// Whether a talk's speaker may still be set: before the gate, and not held for a reason other
/// than an unresolved speaker.
fn speaker_open(state: &str, reason: Option<&str>) -> bool {
    match state {
        "discovered" | "awaiting_consent" => true,
        "needs_review" => matches!(reason, None | Some("speaker_unresolved")),
        _ => false,
    }
}

/// Clears a talk whose speaker was just set when its rights basis is already in place.
async fn clear_if_ready(
    conn: &mut PgConnection,
    talk_id: TalkId,
) -> Result<SpeakerOutcome, RegistryDbError> {
    Ok(if try_clear(conn, talk_id).await?.is_ok() {
        SpeakerOutcome::Cleared
    } else {
        SpeakerOutcome::AwaitingConsent
    })
}

/// Makes each speaker a teacher of `sangha`, with their name as an alias, unless a teacher of
/// that name or alias exists (Spec 03, EngageWisdom sources, rule 3). The sangha is created if
/// new, unlisted until its donation link is checked. Returns the names added.
pub async fn ensure_teachers(
    conn: &mut PgConnection,
    sangha: &str,
    speakers: &[String],
    actor: &str,
) -> Result<Vec<String>, RegistryDbError> {
    sqlx::query!(
        "insert into sangha (name, listed) values ($1, false) on conflict (name) do nothing",
        sangha
    )
    .execute(&mut *conn)
    .await?;
    let mut added = Vec::new();
    for name in speakers {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let known = sqlx::query_scalar!(
            r#"select exists (select 1 from teacher where lower(display_name) = lower($1))
                   or exists (select 1 from teacher_alias where lower(alias) = lower($1)) as "x!""#,
            name
        )
        .fetch_one(&mut *conn)
        .await?;
        if known {
            continue;
        }
        let id = sqlx::query_scalar!(
            "insert into teacher (display_name, sangha_id)
             select $1, id from sangha where name = $2 returning id",
            name,
            sangha
        )
        .fetch_one(&mut *conn)
        .await?;
        sqlx::query!(
            "insert into teacher_alias (teacher_id, alias) values ($1, $2)",
            id,
            name
        )
        .execute(&mut *conn)
        .await?;
        log_change(
            conn,
            &Change {
                entity: "teacher",
                entity_id: id,
                field: "display_name",
                old_value: None,
                new_value: Some(serde_json::json!({ "display_name": name, "sangha": sangha })),
                actor,
                reason: "speaker named by a mandated source",
            },
        )
        .await?;
        added.push(name.to_owned());
    }
    Ok(added)
}

/// Every display name and alias of teachers who have not opted out, for scoring recognition of
/// teachers' names.
pub async fn teacher_names(conn: &mut sqlx::PgConnection) -> Result<Vec<String>, DbError> {
    let names = sqlx::query_scalar!(
        r#"select display_name as "n!" from teacher where opted_out_at is null
           union
           select a.alias from teacher_alias a join teacher t on t.id = a.teacher_id
           where t.opted_out_at is null
           order by 1"#
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(names)
}

/// What `wt teacher cite` changed for one teacher.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CitationChange {
    /// The teacher's display name.
    pub teacher: String,
    /// The citation name now stored.
    pub citation_name: Option<String>,
    /// The sort key now stored.
    pub citation_sort: Option<String>,
    /// Whether anything changed; rerunning with the same values changes nothing.
    pub changed: bool,
}

/// Sets how References enter and sort a teacher (Spec 05, Answer assembly, rule 5), with a
/// `correction_log` row per field that changes. An empty value clears the field, so the
/// teacher is entered by display name again.
pub async fn set_citation(
    conn: &mut PgConnection,
    teacher: &str,
    citation_name: Option<&str>,
    citation_sort: Option<&str>,
    actor: &str,
    reason: &str,
) -> Result<CitationChange, RegistryDbError> {
    let mut tx = sqlx::Acquire::begin(&mut *conn).await?;
    let (id, name) = find_teacher(&mut tx, teacher).await?;
    let row = sqlx::query!(
        "select citation_name, citation_sort from teacher where id = $1 for update",
        id.as_uuid()
    )
    .fetch_one(&mut *tx)
    .await?;
    let clean = |v: Option<&str>| {
        v.map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let (new_name, new_sort) = (clean(citation_name), clean(citation_sort));
    let mut changed = false;
    for (field, old, new) in [
        ("citation_name", &row.citation_name, &new_name),
        ("citation_sort", &row.citation_sort, &new_sort),
    ] {
        if old != new {
            log_change(
                &mut tx,
                &Change {
                    entity: "teacher",
                    entity_id: id.as_uuid(),
                    field,
                    old_value: old.as_ref().map(|v| serde_json::json!(v)),
                    new_value: new.as_ref().map(|v| serde_json::json!(v)),
                    actor,
                    reason,
                },
            )
            .await?;
            changed = true;
        }
    }
    if changed {
        sqlx::query!(
            "update teacher set citation_name = $2, citation_sort = $3 where id = $1",
            id.as_uuid(),
            new_name,
            new_sort
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(CitationChange {
        teacher: name,
        citation_name: new_name,
        citation_sort: new_sort,
        changed,
    })
}
