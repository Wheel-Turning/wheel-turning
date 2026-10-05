//! Typed ids, one newtype per table, so ids of different tables cannot be mixed.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! define_ids {
    ($($(#[$doc:meta])* $name:ident),+ $(,)?) => {
        $(
            $(#[$doc])*
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
            #[serde(transparent)]
            pub struct $name(Uuid);

            impl $name {
                /// Wraps an existing uuid.
                pub const fn from_uuid(id: Uuid) -> Self {
                    Self(id)
                }

                /// The underlying uuid.
                pub const fn as_uuid(&self) -> Uuid {
                    self.0
                }
            }

            impl From<Uuid> for $name {
                fn from(id: Uuid) -> Self {
                    Self(id)
                }
            }

            impl std::str::FromStr for $name {
                type Err = uuid::Error;

                fn from_str(s: &str) -> Result<Self, Self::Err> {
                    Uuid::parse_str(s).map(Self)
                }
            }

            impl From<$name> for Uuid {
                fn from(id: $name) -> Self {
                    id.0
                }
            }

            impl std::fmt::Display for $name {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    self.0.fmt(f)
                }
            }
        )+
    };
}

define_ids! {
    /// Id of a `sangha` row.
    SanghaId,
    /// Id of a `teacher` row.
    TeacherId,
    /// Id of a `teacher_alias` row.
    TeacherAliasId,
    /// Id of a `speaker_consent` row.
    SpeakerConsentId,
    /// Id of a `license_evidence` row.
    LicenseEvidenceId,
    /// Id of a `glossary_term` row.
    GlossaryTermId,
    /// Id of a `source_video` row.
    SourceVideoId,
    /// Id of a `talk` row.
    TalkId,
    /// Id of a `transcript` row.
    TranscriptId,
    /// Id of a `segment` row.
    SegmentId,
    /// Id of a `passage` row.
    PassageId,
    /// Id of a `work` row.
    WorkId,
    /// Id of a `work_locator` row.
    WorkLocatorId,
    /// Id of a `citation` row.
    CitationId,
    /// Id of a `job` row.
    JobId,
    /// Id of a `tester` row.
    TesterId,
    /// Id of a `query_log` row.
    QueryLogId,
    /// Id of an `answer` row.
    AnswerId,
    /// Id of a `feedback` row.
    FeedbackId,
    /// Id of a `dana_event` row.
    DanaEventId,
    /// Id of a `correction_log` row.
    CorrectionLogId,
    /// Id of a `takedown_log` row.
    TakedownLogId,
    /// Id of an `eval_set` row.
    EvalSetId,
    /// Id of an `eval_question` row.
    EvalQuestionId,
    /// Id of an `eval_run` row.
    EvalRunId,
    /// Id of an `eval_rating` row.
    EvalRatingId,
    /// Id of a `claim_audit` row.
    ClaimAuditId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_through_uuid() {
        let raw = Uuid::new_v4();
        let talk = TalkId::from(raw);
        assert_eq!(Uuid::from(talk), raw);
        assert_eq!(talk.to_string(), raw.to_string());
    }

    #[test]
    fn ids_serialize_as_plain_uuid_strings() {
        let raw = Uuid::new_v4();
        let json = serde_json::to_string(&PassageId::from_uuid(raw)).unwrap();
        assert_eq!(json, format!("\"{raw}\""));
    }
}
