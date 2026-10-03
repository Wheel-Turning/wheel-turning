//! `TalkRepo`: loading talks into the gate's typestate and the only ways to move them past it.

use wt_core::gate::{
    ConsentStatus, Consented, CurrentConsent, GateError, LicenseEvidence, Pending, PipelineState,
    RightsBasis, RightsHolder, Talk, TalkState,
};
use wt_core::ids::{LicenseEvidenceId, SanghaId, TalkId, TeacherId};
use sqlx::PgConnection;

use crate::DbError;

/// Errors from moving a talk.
#[derive(Debug, thiserror::Error)]
pub enum TalkError {
    /// No talk with that id.
    #[error("talk {0} does not exist")]
    NotFound(TalkId),
    /// The talk is not in a state this move starts from.
    #[error("talk {id} is {state}, which cannot move to {target}")]
    WrongState {
        /// The talk.
        id: TalkId,
        /// Its state now.
        state: String,
        /// Where the move wanted it.
        target: TalkState,
    },
    /// The database refused.
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<sqlx::Error> for TalkError {
    fn from(e: sqlx::Error) -> Self {
        TalkError::Db(DbError::Sqlx(e))
    }
}

/// The talk repository. Every state change past the gate takes a `Talk<Consented>`.
pub struct TalkRepo;

impl TalkRepo {
    /// Loads a talk. Loading never implies consent: the result is always `Talk<Pending>`.
    pub async fn load(conn: &mut PgConnection, id: TalkId) -> Result<Talk<Pending>, TalkError> {
        let row = sqlx::query!(
            r#"select teacher_id, sangha_id, state::text as "state!" from talk where id = $1"#,
            id.as_uuid()
        )
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(TalkError::NotFound(id))?;
        let state = TalkState::parse(&row.state).ok_or_else(|| TalkError::WrongState {
            id,
            state: row.state.clone(),
            target: TalkState::Discovered,
        })?;
        Ok(Talk::pending(
            id,
            row.teacher_id.map(TeacherId::from),
            row.sangha_id.map(SanghaId::from),
            state,
        ))
    }

    /// Loads a talk and runs the gate against its current consent, license and rights holder.
    /// The outer result is a database failure; the inner one is the gate's answer.
    pub async fn authorize(
        conn: &mut PgConnection,
        id: TalkId,
    ) -> Result<Result<Talk<Consented>, GateError>, TalkError> {
        let talk = Self::load(conn, id).await?;
        let facts = sqlx::query!(
            r#"select t.rights_holder, t.rights_basis::text as rights_basis,
                      le.id as "license_id?", le.license_code as "license_code?",
                      le.reviewed_at is not null as "license_reviewed?", le.allowed as "license_allowed?",
                      cc.status::text as consent_status
               from talk t
               left join license_evidence le on le.id = t.license_evidence_id
               left join current_consent cc on cc.teacher_id = t.teacher_id
               where t.id = $1"#,
            id.as_uuid()
        )
        .fetch_one(&mut *conn)
        .await?;

        let rights = facts.rights_holder.as_deref().and_then(RightsHolder::new);
        let Some(teacher_id) = talk.teacher_id else {
            // The gate reports NoSpeaker; the basis does not matter.
            let placeholder = CurrentConsent::none_recorded(TeacherId::from(uuid_nil()));
            return Ok(talk.authorize(RightsBasis::Consent(&placeholder), rights.as_ref()));
        };

        if facts.rights_basis.as_deref() == Some("license") {
            let evidence = match (facts.license_id, facts.license_code) {
                (Some(id), Some(code)) => LicenseEvidence {
                    id: LicenseEvidenceId::from(id),
                    license_code: code,
                    reviewed: facts.license_reviewed.unwrap_or(false),
                    allowed: facts.license_allowed.unwrap_or(false),
                },
                _ => return Ok(Err(GateError::LicenseUnverified)),
            };
            return Ok(talk.authorize(RightsBasis::License(&evidence), rights.as_ref()));
        }

        let consent = CurrentConsent {
            teacher_id,
            status: facts
                .consent_status
                .as_deref()
                .and_then(ConsentStatus::parse)
                .unwrap_or(ConsentStatus::Unknown),
        };
        Ok(talk.authorize(RightsBasis::Consent(&consent), rights.as_ref()))
    }

    /// The only way into `cleared`: from `awaiting_consent`, for a talk that passed the gate.
    pub async fn clear(conn: &mut PgConnection, talk: Talk<Consented>) -> Result<(), TalkError> {
        let updated = sqlx::query!(
            r#"update talk set state = 'cleared', state_reason = null,
                      rights_basis = coalesce(rights_basis, 'speaker_consent')
               where id = $1 and state = 'awaiting_consent'"#,
            talk.id.as_uuid()
        )
        .execute(&mut *conn)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(Self::wrong_state(conn, talk.id, TalkState::Cleared).await);
        }
        Ok(())
    }

    /// Advances a consented talk to a pipeline state. Only talks already past the gate move.
    pub async fn advance(
        conn: &mut PgConnection,
        talk: &Talk<Consented>,
        next: PipelineState,
    ) -> Result<(), TalkError> {
        let target = TalkState::from(next);
        let updated = sqlx::query!(
            r#"update talk set state = $2::text::talk_state, state_reason = null
               where id = $1
                 and state in ('cleared','fetching','transcribing','diarizing','segmenting',
                               'correcting','extracting','resolving','indexing','ready')"#,
            talk.id.as_uuid(),
            target.as_str()
        )
        .execute(&mut *conn)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(Self::wrong_state(conn, talk.id, target).await);
        }
        Ok(())
    }

    async fn wrong_state(conn: &mut PgConnection, id: TalkId, target: TalkState) -> TalkError {
        match sqlx::query_scalar!(r#"select state::text as "s!" from talk where id = $1"#, id.as_uuid())
            .fetch_optional(&mut *conn)
            .await
        {
            Ok(Some(state)) => TalkError::WrongState { id, state, target },
            Ok(None) => TalkError::NotFound(id),
            Err(e) => e.into(),
        }
    }
}

fn uuid_nil() -> sqlx::types::Uuid {
    sqlx::types::Uuid::nil()
}
