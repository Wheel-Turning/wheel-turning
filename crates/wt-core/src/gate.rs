//! The Rust half of the consent gate (Spec 03). The database half is the
//! `past_gate_requires_consent` constraint (Spec 02); both must agree.
//!
//! A talk loaded from the database is a `Talk<Pending>`. The only way to get a
//! `Talk<Consented>` is [`Talk::authorize`], and every function that fetches media, writes a
//! transcript, segments, extracts, embeds, indexes or advances a talk takes a
//! `&Talk<Consented>`. A consent change between stages stops the pipeline because every job
//! authorizes again before it runs.

use std::fmt;
use std::marker::PhantomData;

use crate::ids::{LicenseEvidenceId, SanghaId, TalkId, TeacherId};

/// Typestate marker: the talk has not passed the gate.
#[derive(Debug, Clone, Copy)]
pub struct Pending;

/// Typestate marker: the talk passed the gate when it was authorized.
#[derive(Debug, Clone, Copy)]
pub struct Consented;

/// The lifecycle states of a talk, mirroring the `talk_state` enum in Spec 02.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TalkState {
    /// Found by discovery, not yet classified.
    Discovered,
    /// Classification could not decide something; a person must look.
    NeedsReview,
    /// Classified; waiting for the speaker's consent and a known rights holder.
    AwaitingConsent,
    /// Passed the gate; ready for the pipeline.
    Cleared,
    /// Pipeline: fetching media.
    Fetching,
    /// Pipeline: transcribing.
    Transcribing,
    /// Pipeline: diarizing.
    Diarizing,
    /// Pipeline: segmenting.
    Segmenting,
    /// Pipeline: glossary correction.
    Correcting,
    /// Pipeline: extraction.
    Extracting,
    /// Pipeline: citation resolution.
    Resolving,
    /// Pipeline: chunking, embedding and indexing.
    Indexing,
    /// Indexed and servable.
    Ready,
    /// Held out of serving without deleting anything.
    Quarantined,
    /// Excluded: not a talk, a duplicate, declined, or excluded by hand.
    Excluded,
    /// Taken down. Only the takedown routine sets this.
    Withdrawn,
}

impl TalkState {
    /// Every state, in declaration order.
    pub const ALL: [TalkState; 16] = [
        TalkState::Discovered,
        TalkState::NeedsReview,
        TalkState::AwaitingConsent,
        TalkState::Cleared,
        TalkState::Fetching,
        TalkState::Transcribing,
        TalkState::Diarizing,
        TalkState::Segmenting,
        TalkState::Correcting,
        TalkState::Extracting,
        TalkState::Resolving,
        TalkState::Indexing,
        TalkState::Ready,
        TalkState::Quarantined,
        TalkState::Excluded,
        TalkState::Withdrawn,
    ];

    /// The database spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            TalkState::Discovered => "discovered",
            TalkState::NeedsReview => "needs_review",
            TalkState::AwaitingConsent => "awaiting_consent",
            TalkState::Cleared => "cleared",
            TalkState::Fetching => "fetching",
            TalkState::Transcribing => "transcribing",
            TalkState::Diarizing => "diarizing",
            TalkState::Segmenting => "segmenting",
            TalkState::Correcting => "correcting",
            TalkState::Extracting => "extracting",
            TalkState::Resolving => "resolving",
            TalkState::Indexing => "indexing",
            TalkState::Ready => "ready",
            TalkState::Quarantined => "quarantined",
            TalkState::Excluded => "excluded",
            TalkState::Withdrawn => "withdrawn",
        }
    }

    /// Parses the database spelling.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == value)
    }

    /// Whether the state is at or beyond `cleared` and before the terminal states, which is
    /// where the database requires consent.
    pub fn is_past_gate(self) -> bool {
        matches!(
            self,
            TalkState::Cleared
                | TalkState::Fetching
                | TalkState::Transcribing
                | TalkState::Diarizing
                | TalkState::Segmenting
                | TalkState::Correcting
                | TalkState::Extracting
                | TalkState::Resolving
                | TalkState::Indexing
                | TalkState::Ready
        )
    }
}

impl fmt::Display for TalkState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A pipeline state a consented talk may be advanced into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineState {
    /// Fetching media.
    Fetching,
    /// Transcribing.
    Transcribing,
    /// Diarizing.
    Diarizing,
    /// Segmenting.
    Segmenting,
    /// Glossary correction.
    Correcting,
    /// Extraction.
    Extracting,
    /// Citation resolution.
    Resolving,
    /// Indexing.
    Indexing,
    /// Indexed and servable.
    Ready,
}

impl From<PipelineState> for TalkState {
    fn from(state: PipelineState) -> Self {
        match state {
            PipelineState::Fetching => TalkState::Fetching,
            PipelineState::Transcribing => TalkState::Transcribing,
            PipelineState::Diarizing => TalkState::Diarizing,
            PipelineState::Segmenting => TalkState::Segmenting,
            PipelineState::Correcting => TalkState::Correcting,
            PipelineState::Extracting => TalkState::Extracting,
            PipelineState::Resolving => TalkState::Resolving,
            PipelineState::Indexing => TalkState::Indexing,
            PipelineState::Ready => TalkState::Ready,
        }
    }
}

/// A speaker's consent decision, mirroring the `consent_status` enum in Spec 02.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentStatus {
    /// No decision recorded. A teacher with no consent row is treated as unknown.
    Unknown,
    /// Asked, no answer yet.
    Requested,
    /// Consent given.
    Granted,
    /// Consent refused.
    Declined,
    /// Consent withdrawn after being given.
    Withdrawn,
}

impl ConsentStatus {
    /// The database spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ConsentStatus::Unknown => "unknown",
            ConsentStatus::Requested => "requested",
            ConsentStatus::Granted => "granted",
            ConsentStatus::Declined => "declined",
            ConsentStatus::Withdrawn => "withdrawn",
        }
    }

    /// Parses the database spelling.
    pub fn parse(value: &str) -> Option<Self> {
        [
            ConsentStatus::Unknown,
            ConsentStatus::Requested,
            ConsentStatus::Granted,
            ConsentStatus::Declined,
            ConsentStatus::Withdrawn,
        ]
        .into_iter()
        .find(|s| s.as_str() == value)
    }
}

impl fmt::Display for ConsentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The latest consent decision for one teacher, or `Unknown` when there is no row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentConsent {
    /// Whose consent this is.
    pub teacher_id: TeacherId,
    /// The latest status.
    pub status: ConsentStatus,
}

impl CurrentConsent {
    /// The default for a teacher with no consent row: never permission.
    pub fn none_recorded(teacher_id: TeacherId) -> Self {
        Self {
            teacher_id,
            status: ConsentStatus::Unknown,
        }
    }
}

/// A human-reviewed license record (Spec 03, Licensed sources).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseEvidence {
    /// The evidence row.
    pub id: LicenseEvidenceId,
    /// For example `cc-by-4.0`.
    pub license_code: String,
    /// When a person reviewed the snapshot, if anyone has.
    pub reviewed: bool,
    /// Set only by `dg license approve` after review.
    pub allowed: bool,
}

/// What clears a talk: its speaker's consent, or a verified license.
#[derive(Debug, Clone, Copy)]
pub enum RightsBasis<'a> {
    /// Speaker consent, the basis for SSZ talks.
    Consent(&'a CurrentConsent),
    /// A license the publisher states, reviewed and allowed by a person.
    License(&'a LicenseEvidence),
}

/// A known rights holder. The constructor rejects blank values and "unknown" in any case, so a
/// `RightsHolder` always names someone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RightsHolder(String);

impl RightsHolder {
    /// A known rights holder, or `None` when the value is blank or "unknown" in any case.
    pub fn new(value: &str) -> Option<Self> {
        let trimmed = value.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("unknown") {
            None
        } else {
            Some(Self(trimmed.to_owned()))
        }
    }

    /// The rights holder's name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why a talk did not pass the gate.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GateError {
    /// No teacher is attached to the talk.
    #[error("the talk has no speaker")]
    NoSpeaker,
    /// No sangha is attached to the talk, so it cannot be attributed or carry a dana link.
    #[error("the talk has no sangha")]
    NoSangha,
    /// The speaker has not granted consent: no row, unknown, requested, or a row for someone
    /// else.
    #[error("the speaker has not granted consent")]
    ConsentMissing,
    /// The speaker declined.
    #[error("the speaker declined")]
    ConsentDeclined,
    /// The speaker withdrew consent.
    #[error("the speaker withdrew consent")]
    ConsentWithdrawn,
    /// The rights holder is not known.
    #[error("the rights holder is unknown")]
    RightsUnknown,
    /// The license evidence has not been reviewed by a person.
    #[error("the license evidence has not been reviewed")]
    LicenseUnverified,
    /// The license evidence was reviewed and is not allowed.
    #[error("the license is not allowed")]
    LicenseNotAllowed,
}

/// A talk in a typestate. `S` is [`Pending`] or [`Consented`].
#[derive(Debug, Clone)]
pub struct Talk<S> {
    /// The talk.
    pub id: TalkId,
    /// The speaker, once resolved.
    pub teacher_id: Option<TeacherId>,
    /// The sangha the talk was given at, which receives its dana.
    pub sangha_id: Option<SanghaId>,
    /// The state when the talk was loaded.
    pub state: TalkState,
    _s: PhantomData<S>,
}

impl Talk<Pending> {
    /// A talk as loaded from the database. Loading never implies consent.
    pub fn pending(
        id: TalkId,
        teacher_id: Option<TeacherId>,
        sangha_id: Option<SanghaId>,
        state: TalkState,
    ) -> Self {
        Self {
            id,
            teacher_id,
            sangha_id,
            state,
            _s: PhantomData,
        }
    }

    /// The gate. Returns a `Talk<Consented>` only when the talk has a speaker and a sangha, the
    /// rights holder is known, and the basis allows it: the speaker's current consent is
    /// `granted`, or the license evidence was reviewed and allowed.
    pub fn authorize(
        self,
        basis: RightsBasis<'_>,
        rights: Option<&RightsHolder>,
    ) -> Result<Talk<Consented>, GateError> {
        let Some(teacher_id) = self.teacher_id else {
            return Err(GateError::NoSpeaker);
        };
        if self.sangha_id.is_none() {
            return Err(GateError::NoSangha);
        }
        match basis {
            RightsBasis::Consent(consent) => {
                if consent.teacher_id != teacher_id {
                    return Err(GateError::ConsentMissing);
                }
                match consent.status {
                    ConsentStatus::Granted => {}
                    ConsentStatus::Unknown | ConsentStatus::Requested => {
                        return Err(GateError::ConsentMissing);
                    }
                    ConsentStatus::Declined => return Err(GateError::ConsentDeclined),
                    ConsentStatus::Withdrawn => return Err(GateError::ConsentWithdrawn),
                }
            }
            RightsBasis::License(evidence) => {
                if !evidence.reviewed {
                    return Err(GateError::LicenseUnverified);
                }
                if !evidence.allowed {
                    return Err(GateError::LicenseNotAllowed);
                }
            }
        }
        if rights.is_none() {
            return Err(GateError::RightsUnknown);
        }
        Ok(Talk {
            id: self.id,
            teacher_id: self.teacher_id,
            sangha_id: self.sangha_id,
            state: self.state,
            _s: PhantomData,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn talk(teacher: Option<TeacherId>) -> Talk<Pending> {
        Talk::pending(
            TalkId::from(Uuid::new_v4()),
            teacher,
            Some(SanghaId::from(Uuid::new_v4())),
            TalkState::AwaitingConsent,
        )
    }

    fn teacher() -> TeacherId {
        TeacherId::from(Uuid::new_v4())
    }

    fn consent(teacher_id: TeacherId, status: ConsentStatus) -> CurrentConsent {
        CurrentConsent { teacher_id, status }
    }

    fn holder() -> RightsHolder {
        RightsHolder::new("Kanshin").unwrap()
    }

    #[test]
    fn granted_consent_and_a_known_rights_holder_pass() {
        let t = teacher();
        let consented = talk(Some(t))
            .authorize(
                RightsBasis::Consent(&consent(t, ConsentStatus::Granted)),
                Some(&holder()),
            )
            .unwrap();
        assert_eq!(consented.teacher_id, Some(t));
    }

    #[test]
    fn no_speaker() {
        let t = teacher();
        let err = talk(None)
            .authorize(
                RightsBasis::Consent(&consent(t, ConsentStatus::Granted)),
                Some(&holder()),
            )
            .unwrap_err();
        assert_eq!(err, GateError::NoSpeaker);
    }

    #[test]
    fn no_sangha() {
        let t = teacher();
        let pending = Talk::pending(
            TalkId::from(Uuid::new_v4()),
            Some(t),
            None,
            TalkState::AwaitingConsent,
        );
        let err = pending
            .authorize(
                RightsBasis::Consent(&consent(t, ConsentStatus::Granted)),
                Some(&holder()),
            )
            .unwrap_err();
        assert_eq!(err, GateError::NoSangha);
    }

    #[test]
    fn no_consent_row_is_never_permission() {
        let t = teacher();
        let err = talk(Some(t))
            .authorize(
                RightsBasis::Consent(&CurrentConsent::none_recorded(t)),
                Some(&holder()),
            )
            .unwrap_err();
        assert_eq!(err, GateError::ConsentMissing);
    }

    #[test]
    fn requested_is_not_consent() {
        let t = teacher();
        let err = talk(Some(t))
            .authorize(
                RightsBasis::Consent(&consent(t, ConsentStatus::Requested)),
                Some(&holder()),
            )
            .unwrap_err();
        assert_eq!(err, GateError::ConsentMissing);
    }

    #[test]
    fn someone_elses_consent_does_not_count() {
        let err = talk(Some(teacher()))
            .authorize(
                RightsBasis::Consent(&consent(teacher(), ConsentStatus::Granted)),
                Some(&holder()),
            )
            .unwrap_err();
        assert_eq!(err, GateError::ConsentMissing);
    }

    #[test]
    fn declined() {
        let t = teacher();
        let err = talk(Some(t))
            .authorize(
                RightsBasis::Consent(&consent(t, ConsentStatus::Declined)),
                Some(&holder()),
            )
            .unwrap_err();
        assert_eq!(err, GateError::ConsentDeclined);
    }

    #[test]
    fn withdrawn() {
        let t = teacher();
        let err = talk(Some(t))
            .authorize(
                RightsBasis::Consent(&consent(t, ConsentStatus::Withdrawn)),
                Some(&holder()),
            )
            .unwrap_err();
        assert_eq!(err, GateError::ConsentWithdrawn);
    }

    #[test]
    fn rights_unknown() {
        let t = teacher();
        let err = talk(Some(t))
            .authorize(
                RightsBasis::Consent(&consent(t, ConsentStatus::Granted)),
                None,
            )
            .unwrap_err();
        assert_eq!(err, GateError::RightsUnknown);
    }

    #[test]
    fn rights_unknown_in_any_case_or_blank_is_not_a_holder() {
        for value in ["unknown", "Unknown", "UnKnOwN", "  UNKNOWN ", "", "   "] {
            assert_eq!(
                RightsHolder::new(value),
                None,
                "{value:?} must not be a holder"
            );
        }
        let t = teacher();
        let err = talk(Some(t))
            .authorize(
                RightsBasis::Consent(&consent(t, ConsentStatus::Granted)),
                RightsHolder::new("UnKnown").as_ref(),
            )
            .unwrap_err();
        assert_eq!(err, GateError::RightsUnknown);
    }

    #[test]
    fn license_basis_needs_review_and_approval() {
        let evidence = |reviewed, allowed| LicenseEvidence {
            id: LicenseEvidenceId::from(Uuid::new_v4()),
            license_code: "cc-by-4.0".to_owned(),
            reviewed,
            allowed,
        };
        let t = teacher();
        assert_eq!(
            talk(Some(t))
                .authorize(
                    RightsBasis::License(&evidence(false, false)),
                    Some(&holder())
                )
                .unwrap_err(),
            GateError::LicenseUnverified
        );
        assert_eq!(
            talk(Some(t))
                .authorize(
                    RightsBasis::License(&evidence(true, false)),
                    Some(&holder())
                )
                .unwrap_err(),
            GateError::LicenseNotAllowed
        );
        assert!(
            talk(Some(t))
                .authorize(RightsBasis::License(&evidence(true, true)), Some(&holder()))
                .is_ok()
        );
    }

    #[test]
    fn state_spellings_round_trip() {
        for state in TalkState::ALL {
            assert_eq!(TalkState::parse(state.as_str()), Some(state));
        }
        assert!(TalkState::Cleared.is_past_gate());
        assert!(TalkState::Ready.is_past_gate());
        assert!(!TalkState::AwaitingConsent.is_past_gate());
        assert!(!TalkState::Excluded.is_past_gate());
    }
}
