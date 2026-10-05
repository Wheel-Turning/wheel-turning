//! Consent records and their propagation to talks, rights holders, and manual exclusion
//! (Spec 03: the consent gate in the pipeline).

use sqlx::{PgConnection, PgPool};
use wt_core::gate::{ConsentStatus, GateError};
use wt_core::ids::{TalkId, TeacherId};

use crate::DbError;
use crate::correction::log_change;
use crate::registry::{RegistryDbError, find_teacher};
use crate::takedown::{self, Scope, TakedownError, TakedownReport};
use crate::talk::{TalkError, TalkRepo};

/// Consent command errors.
#[derive(Debug, thiserror::Error)]
pub enum ConsentError {
    /// The takedown a decline or withdrawal runs failed, so nothing was recorded.
    #[error(transparent)]
    Takedown(#[from] TakedownError),
    /// The teacher or talk could not be found.
    #[error(transparent)]
    Registry(#[from] RegistryDbError),
    /// A talk move failed.
    #[error(transparent)]
    Talk(#[from] TalkError),
    /// The database refused.
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<sqlx::Error> for ConsentError {
    fn from(e: sqlx::Error) -> Self {
        ConsentError::Db(DbError::Sqlx(e))
    }
}

/// What a consent change did to the teacher's talks.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Propagation {
    /// Talks moved to `cleared`.
    pub cleared: Vec<TalkId>,
    /// The teacher takedown a decline or withdrawal ran.
    pub takedown: Option<TakedownReport>,
    /// Talks moved back to `awaiting_consent` because consent is no longer granted.
    pub returned_to_awaiting: Vec<TalkId>,
    /// Talks left waiting, with the gate's reason (for example a rights holder still unknown).
    pub still_waiting: Vec<(TalkId, String)>,
}

/// A row of `dg consent list`.
#[derive(Debug, Clone)]
pub struct ConsentRow {
    /// Teacher display name.
    pub teacher: String,
    /// Latest status, `unknown` when no row exists.
    pub status: String,
    /// Pointer to the stored reply.
    pub evidence_ref: Option<String>,
    /// When the latest row was written.
    pub recorded_at: Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>>,
    /// Number of the teacher's talks in each state, `state=count` joined with commas.
    pub talks: String,
}

/// `wt consent set`: appends a `speaker_consent` row and applies it to every talk of the teacher
/// in one transaction. `declined` and `withdrawn` run the teacher takedown in that transaction
/// (Spec 03, Mandated talks, rule 5), so the material is deleted, not only excluded.
pub async fn set_consent(
    pool: &PgPool,
    teacher: &str,
    status: ConsentStatus,
    evidence_ref: Option<&str>,
    scope_note: Option<&str>,
    actor: &str,
) -> Result<Propagation, ConsentError> {
    let mut tx = pool.begin().await?;
    let (teacher_id, display_name) = find_teacher(&mut tx, teacher).await?;
    sqlx::query!(
        r#"insert into speaker_consent (teacher_id, status, scope_note, evidence_ref, decided_at, created_by)
           values ($1, $2::text::consent_status, $3, $4, now(), $5)"#,
        teacher_id.as_uuid(),
        status.as_str(),
        scope_note,
        evidence_ref,
        actor
    )
    .execute(&mut *tx)
    .await?;
    let propagation = match status {
        ConsentStatus::Declined | ConsentStatus::Withdrawn => {
            let report =
                takedown::run_in(&mut tx, &Scope::Teacher(display_name), actor, Some(status))
                    .await?;
            Propagation {
                takedown: Some(report),
                ..Propagation::default()
            }
        }
        _ => propagate(&mut tx, teacher_id, status, actor).await?,
    };
    tx.commit().await?;
    Ok(propagation)
}

async fn propagate(
    conn: &mut PgConnection,
    teacher_id: TeacherId,
    status: ConsentStatus,
    actor: &str,
) -> Result<Propagation, ConsentError> {
    let mut out = Propagation::default();
    let reason = format!("consent {status}");

    match status {
        ConsentStatus::Granted => {
            sqlx::query!(
                r#"update talk set consent_status = 'granted',
                          rights_basis = coalesce(rights_basis, 'speaker_consent')
                   where teacher_id = $1 and state <> 'withdrawn'"#,
                teacher_id.as_uuid()
            )
            .execute(&mut *conn)
            .await?;
            let waiting = sqlx::query_scalar!(
                "select id from talk where teacher_id = $1 and state = 'awaiting_consent' order by id",
                teacher_id.as_uuid()
            )
            .fetch_all(&mut *conn)
            .await?;
            for id in waiting {
                let talk_id = TalkId::from(id);
                match try_clear(conn, talk_id).await? {
                    Ok(()) => {
                        log_state(conn, id, "awaiting_consent", "cleared", actor, &reason).await?;
                        out.cleared.push(talk_id);
                    }
                    Err(gate) => out
                        .still_waiting
                        .push((talk_id, gate_reason(&gate).to_owned())),
                }
            }
        }
        ConsentStatus::Requested | ConsentStatus::Unknown => {
            // The database forbids a talk past the gate without granted consent, so any talk
            // already cleared or in the pipeline goes back to wait.
            let returned = sqlx::query!(
                r#"update talk t set state = 'awaiting_consent', state_reason = $2,
                          consent_status = $3::text::consent_status
                   from (select id, state::text as old_state from talk
                         where teacher_id = $1
                           and state in ('cleared','fetching','transcribing','diarizing','segmenting',
                                         'correcting','extracting','resolving','indexing','ready')
                         for update) old
                   where t.id = old.id
                   returning t.id, old.old_state as "old_state!""#,
                teacher_id.as_uuid(),
                format!("consent_{status}"),
                status.as_str()
            )
            .fetch_all(&mut *conn)
            .await?;
            for row in returned {
                log_state(
                    conn,
                    row.id,
                    &row.old_state,
                    "awaiting_consent",
                    actor,
                    &reason,
                )
                .await?;
                out.returned_to_awaiting.push(TalkId::from(row.id));
            }
            sqlx::query!(
                r#"update talk set consent_status = $2::text::consent_status
                   where teacher_id = $1 and state <> 'withdrawn'"#,
                teacher_id.as_uuid(),
                status.as_str()
            )
            .execute(&mut *conn)
            .await?;
        }
        // set_consent runs the takedown for these instead.
        ConsentStatus::Declined | ConsentStatus::Withdrawn => {}
    }
    Ok(out)
}

/// Runs the gate for one talk in `awaiting_consent` and clears it if it passes. The inner
/// result is the gate's answer; a talk that fails keeps its state with the reason recorded.
pub(crate) async fn try_clear(
    conn: &mut PgConnection,
    talk_id: TalkId,
) -> Result<Result<(), GateError>, TalkError> {
    match TalkRepo::authorize(conn, talk_id).await? {
        Ok(consented) => {
            TalkRepo::clear(conn, consented).await?;
            Ok(Ok(()))
        }
        Err(gate) => {
            sqlx::query!(
                "update talk set state_reason = $2 where id = $1 and state = 'awaiting_consent'",
                talk_id.as_uuid(),
                gate_reason(&gate)
            )
            .execute(&mut *conn)
            .await?;
            Ok(Err(gate))
        }
    }
}

/// The `state_reason` recorded for a gate refusal.
pub fn gate_reason(gate: &GateError) -> &'static str {
    match gate {
        GateError::NoSpeaker => "speaker_unresolved",
        GateError::NoSangha => "sangha_unknown",
        GateError::ConsentMissing => "consent_missing",
        GateError::ConsentDeclined => "declined",
        GateError::ConsentWithdrawn => "withdrawn",
        GateError::RightsUnknown => "rights_unknown",
        GateError::LicenseUnverified => "license_unverified",
        GateError::LicenseNotAllowed => "license_not_allowed",
    }
}

async fn log_state(
    conn: &mut PgConnection,
    talk: sqlx::types::Uuid,
    old: &str,
    new: &str,
    actor: &str,
    reason: &str,
) -> Result<(), DbError> {
    log_change(
        conn,
        "talk",
        talk,
        "state",
        Some(serde_json::json!(old)),
        Some(serde_json::json!(new)),
        actor,
        reason,
    )
    .await
}

/// `dg consent list`: every teacher with their latest consent and talk counts by state.
pub async fn list_consent(pool: &PgPool) -> Result<Vec<ConsentRow>, ConsentError> {
    let rows = sqlx::query!(
        r#"select te.display_name, coalesce(cc.status::text, 'unknown') as "status!",
                  cc.evidence_ref, cc.created_at as "recorded_at?",
                  coalesce((select string_agg(s || '=' || n, ', ' order by s)
                            from (select state::text as s, count(*) as n from talk
                                  where teacher_id = te.id group by state) x), '') as "talks!"
           from teacher te left join current_consent cc on cc.teacher_id = te.id
           order by te.display_name"#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ConsentRow {
            teacher: r.display_name,
            status: r.status,
            evidence_ref: r.evidence_ref,
            recorded_at: r.recorded_at,
            talks: r.talks,
        })
        .collect())
}

/// Which talks a rights change applies to.
#[derive(Debug, Clone)]
pub enum RightsTarget {
    /// One talk.
    Talk(TalkId),
    /// Every talk of a teacher (by display name or alias).
    Teacher(String),
}

/// The rights holder to record.
#[derive(Debug, Clone)]
pub enum Holder {
    /// The speaker: writes the teacher's display name.
    Speaker,
    /// A named holder, for example a foundation.
    Named(String),
}

/// `dg talk set-rights`: sets the rights holder, logs the change, and clears any talk that now
/// passes the gate. Returns the talks that were cleared.
pub async fn set_rights(
    pool: &PgPool,
    target: RightsTarget,
    holder: Holder,
    actor: &str,
) -> Result<Vec<TalkId>, ConsentError> {
    let mut tx = pool.begin().await?;
    let talks = match &target {
        RightsTarget::Talk(id) => sqlx::query!(
            r#"select t.id, t.rights_holder, te.display_name as "teacher?"
               from talk t left join teacher te on te.id = t.teacher_id where t.id = $1"#,
            id.as_uuid()
        )
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|r| (r.id, r.rights_holder, r.teacher))
        .collect::<Vec<_>>(),
        RightsTarget::Teacher(name) => {
            let (teacher_id, display) = find_teacher(&mut tx, name).await?;
            sqlx::query!(
                "select id, rights_holder from talk where teacher_id = $1 and state <> 'withdrawn'",
                teacher_id.as_uuid()
            )
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .map(|r| (r.id, r.rights_holder, Some(display.clone())))
            .collect()
        }
    };
    if talks.is_empty()
        && let RightsTarget::Talk(id) = target
    {
        return Err(TalkError::NotFound(id).into());
    }

    let mut cleared = Vec::new();
    for (id, old, teacher) in talks {
        let new = match &holder {
            Holder::Named(name) => name.trim().to_owned(),
            Holder::Speaker => match teacher {
                Some(name) => name,
                None => {
                    return Err(ConsentError::Registry(RegistryDbError::UnknownTeacher(
                        format!("speaker of talk {id}"),
                    )));
                }
            },
        };
        sqlx::query!("update talk set rights_holder = $2 where id = $1", id, new)
            .execute(&mut *tx)
            .await?;
        log_change(
            &mut tx,
            "talk",
            id,
            "rights_holder",
            old.map(serde_json::Value::from),
            Some(serde_json::Value::from(new)),
            actor,
            "set-rights",
        )
        .await?;
        let state =
            sqlx::query_scalar!(r#"select state::text as "s!" from talk where id = $1"#, id)
                .fetch_one(&mut *tx)
                .await?;
        if state == "awaiting_consent" && try_clear(&mut tx, TalkId::from(id)).await?.is_ok() {
            cleared.push(TalkId::from(id));
        }
    }
    tx.commit().await?;
    Ok(cleared)
}

/// `dg talk exclude`: excludes one talk by hand, which is how a speaker's limits apply in v1.
pub async fn exclude_talk(
    pool: &PgPool,
    talk_id: TalkId,
    reason: &str,
    actor: &str,
) -> Result<(), ConsentError> {
    let mut tx = pool.begin().await?;
    let old = sqlx::query_scalar!(
        r#"select state::text as "s!" from talk where id = $1 for update"#,
        talk_id.as_uuid()
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(TalkError::NotFound(talk_id))?;
    if old == "withdrawn" {
        return Err(TalkError::WrongState {
            id: talk_id,
            state: old,
            target: wt_core::gate::TalkState::Excluded,
        }
        .into());
    }
    sqlx::query!(
        "update talk set state = 'excluded', state_reason = $2 where id = $1",
        talk_id.as_uuid(),
        reason
    )
    .execute(&mut *tx)
    .await?;
    log_state(&mut tx, talk_id.as_uuid(), &old, "excluded", actor, reason).await?;
    tx.commit().await?;
    Ok(())
}
