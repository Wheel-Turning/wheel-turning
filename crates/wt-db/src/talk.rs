//! `TalkRepo`: loading talks into the gate's typestate and the only ways to move them past it.

use sqlx::PgConnection;
use wt_core::gate::{
    ConsentStatus, Consented, CurrentConsent, GateError, Granted, LicenseEvidence, MandateEvidence,
    Pending, PipelineState, RightsBasis, RightsHolder, Standing, Talk, TalkState,
};
use wt_core::ids::{LicenseEvidenceId, SanghaId, TalkId, TeacherId};

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

    /// Loads a talk and runs the gate against its current rights: the speaker's standing (opted
    /// out or excluded), the rights holder, and one basis. A talk on a license or a mandate is
    /// judged on it. Otherwise granted, declined or withdrawn consent decides; with no answer
    /// from the speaker, a talk from a mandated talk source is judged on that mandate (Spec 03,
    /// Mandated talks). The outer result is a database failure; the inner one is the gate's
    /// answer.
    pub async fn authorize(
        conn: &mut PgConnection,
        id: TalkId,
    ) -> Result<Result<Talk<Consented>, GateError>, TalkError> {
        let talk = Self::load(conn, id).await?;
        let facts = sqlx::query!(
            r#"select t.rights_holder, t.rights_basis::text as rights_basis,
                      le.id as "license_id?", le.license_code as "license_code?",
                      le.reviewed_at is not null as "license_reviewed?", le.allowed as "license_allowed?",
                      cc.status::text as consent_status,
                      te.opted_out_at is not null as "opted_out?",
                      exists (select 1 from source_exclusion x
                              where x.scope = 'person' and x.lifted_at is null
                                and lower(x.pattern) = lower(te.display_name)) as "excluded!",
                      sp.basis as "source_basis?", sp.enabled as "source_enabled?",
                      sp.license_evidence_id as "source_evidence?",
                      sle.allowed as "source_evidence_allowed?",
                      (select value from rights_setting where key = 'talks_basis') = 'mandate'
                        as "talks_basis_mandate!"
               from talk t
               left join license_evidence le on le.id = t.license_evidence_id
               left join current_consent cc on cc.teacher_id = t.teacher_id
               left join teacher te on te.id = t.teacher_id
               left join source_policy sp on sp.key = t.source_key and sp.kind = 'talks'
               left join license_evidence sle on sle.id = sp.license_evidence_id
               where t.id = $1"#,
            id.as_uuid()
        )
        .fetch_one(&mut *conn)
        .await?;

        let rights = facts.rights_holder.as_deref().and_then(RightsHolder::new);
        let standing = Standing {
            opted_out: facts.opted_out.unwrap_or(false),
            excluded: facts.excluded,
        };
        let Some(teacher_id) = talk.teacher_id else {
            // The gate reports NoSpeaker; the basis does not matter.
            let placeholder = CurrentConsent::none_recorded(TeacherId::from(uuid_nil()));
            return Ok(talk.authorize(
                RightsBasis::Consent(&placeholder),
                rights.as_ref(),
                standing,
            ));
        };

        match facts.rights_basis.as_deref() {
            Some("license") => {
                let evidence = match (facts.license_id, facts.license_code) {
                    (Some(id), Some(code)) => LicenseEvidence {
                        id: LicenseEvidenceId::from(id),
                        license_code: code,
                        reviewed: facts.license_reviewed.unwrap_or(false),
                        allowed: facts.license_allowed.unwrap_or(false),
                    },
                    _ => return Ok(Err(GateError::LicenseUnverified)),
                };
                return Ok(talk.authorize(
                    RightsBasis::License(&evidence),
                    rights.as_ref(),
                    standing,
                ));
            }
            Some("mandate") => {
                let Some(evidence) = facts.license_id else {
                    return Ok(Err(GateError::MandateOff));
                };
                let mandate = MandateEvidence {
                    id: LicenseEvidenceId::from(evidence),
                    allowed: facts.license_allowed.unwrap_or(false),
                    talks_basis_mandate: facts.talks_basis_mandate,
                    // A talk with no source of record rests on its own evidence.
                    source_enabled: facts.source_enabled.unwrap_or(true),
                };
                return Ok(talk.authorize(
                    RightsBasis::Mandate(&mandate),
                    rights.as_ref(),
                    standing,
                ));
            }
            _ => {}
        }

        let consent = CurrentConsent {
            teacher_id,
            status: facts
                .consent_status
                .as_deref()
                .and_then(ConsentStatus::parse)
                .unwrap_or(ConsentStatus::Unknown),
        };
        let unanswered = matches!(
            consent.status,
            ConsentStatus::Unknown | ConsentStatus::Requested
        );
        if unanswered
            && facts.source_basis.as_deref() == Some("mandate")
            && let Some(evidence) = facts.source_evidence
        {
            let mandate = MandateEvidence {
                id: LicenseEvidenceId::from(evidence),
                allowed: facts.source_evidence_allowed.unwrap_or(false),
                talks_basis_mandate: facts.talks_basis_mandate,
                source_enabled: facts.source_enabled.unwrap_or(false),
            };
            return Ok(talk.authorize(RightsBasis::Mandate(&mandate), rights.as_ref(), standing));
        }
        Ok(talk.authorize(RightsBasis::Consent(&consent), rights.as_ref(), standing))
    }

    /// The only way into `cleared`: from `awaiting_consent`, for a talk that passed the gate.
    /// It records the basis the talk passed on, and for a mandate the mandate's evidence.
    pub async fn clear(conn: &mut PgConnection, talk: Talk<Consented>) -> Result<(), TalkError> {
        let (basis, evidence) = match talk.granted() {
            Some(Granted::Mandate(id)) => ("mandate", Some(id.as_uuid())),
            Some(Granted::License(_)) => ("license", None),
            Some(Granted::Consent) | None => ("speaker_consent", None),
        };
        let updated = sqlx::query!(
            r#"update talk set state = 'cleared', state_reason = null,
                      rights_basis = $2::text::rights_basis,
                      license_evidence_id = coalesce($3, license_evidence_id),
                      -- The gate read granted consent; the talk's copy may predate it.
                      consent_status = case when $2 = 'speaker_consent' then 'granted'
                                            else consent_status end
               where id = $1 and state = 'awaiting_consent'"#,
            talk.id.as_uuid(),
            basis,
            evidence
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

    /// The error for a transition the talk's state does not allow: its actual state, or not found
    /// when it is gone.
    async fn wrong_state(conn: &mut PgConnection, id: TalkId, target: TalkState) -> TalkError {
        match sqlx::query_scalar!(
            r#"select state::text as "s!" from talk where id = $1"#,
            id.as_uuid()
        )
        .fetch_optional(&mut *conn)
        .await
        {
            Ok(Some(state)) => TalkError::WrongState { id, state, target },
            Ok(None) => TalkError::NotFound(id),
            Err(e) => e.into(),
        }
    }
}

/// The nil id, for the placeholder consent of a talk with no speaker, which the gate refuses for
/// its missing speaker before the consent matters.
fn uuid_nil() -> sqlx::types::Uuid {
    sqlx::types::Uuid::nil()
}
