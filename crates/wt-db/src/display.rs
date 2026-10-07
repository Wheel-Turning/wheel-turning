//! Display levels (Spec 03, Display levels; owner's decisions of October 7, 2026): how much of
//! a teacher's talks the public sees, and what the project keeps. A talk may be set narrower
//! than its teacher, never wider. Every change is logged in `correction_log`.

use serde::Serialize;
use sqlx::PgConnection;
use wt_core::ids::TalkId;

use crate::DbError;
use crate::correction::log_change;
use crate::registry::{RegistryDbError, find_teacher};

/// A display level, from narrowest to widest by [`DisplayLevel::rank`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayLevel {
    /// f: everything removed, with permanent exclusions (`wt teacher opt-out`).
    Removed,
    /// e: a copy for the teacher, then the takedown.
    ExportThenDelete,
    /// a: nothing public; everything kept and restorable.
    Excluded,
    /// d: nothing public; everything kept for the teacher's use, with a copy for them.
    OwnerOnly,
    /// b: passages in answers, no transcript page.
    Excerpts,
    /// c: passages and the transcript page. The default.
    Full,
}

impl DisplayLevel {
    /// Every level, narrowest first.
    pub const ALL: [DisplayLevel; 6] = [
        DisplayLevel::Removed,
        DisplayLevel::ExportThenDelete,
        DisplayLevel::Excluded,
        DisplayLevel::OwnerOnly,
        DisplayLevel::Excerpts,
        DisplayLevel::Full,
    ];

    /// The database value.
    pub fn as_str(self) -> &'static str {
        match self {
            DisplayLevel::Removed => "removed",
            DisplayLevel::ExportThenDelete => "export_then_delete",
            DisplayLevel::Excluded => "excluded",
            DisplayLevel::OwnerOnly => "owner_only",
            DisplayLevel::Excerpts => "excerpts",
            DisplayLevel::Full => "full",
        }
    }

    /// The PRD's letter.
    pub fn letter(self) -> char {
        match self {
            DisplayLevel::Excluded => 'a',
            DisplayLevel::Excerpts => 'b',
            DisplayLevel::Full => 'c',
            DisplayLevel::OwnerOnly => 'd',
            DisplayLevel::ExportThenDelete => 'e',
            DisplayLevel::Removed => 'f',
        }
    }

    /// From a database value or the PRD's letter.
    pub fn parse(value: &str) -> Option<Self> {
        let v = value.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|l| l.as_str() == v || v.len() == 1 && v.starts_with(l.letter()))
    }

    /// 0 for the narrowest, 5 for full.
    pub fn rank(self) -> u8 {
        self as u8
    }

    /// Whether the public sees anything: passages at least.
    pub fn is_public(self) -> bool {
        matches!(self, DisplayLevel::Excerpts | DisplayLevel::Full)
    }
}

impl std::fmt::Display for DisplayLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.letter(), self.as_str())
    }
}

/// Why a level could not be set.
#[derive(Debug, thiserror::Error)]
pub enum DisplayError {
    /// The teacher is not in the registry.
    #[error(transparent)]
    Registry(#[from] RegistryDbError),
    /// The database refused.
    #[error(transparent)]
    Db(#[from] DbError),
    /// A connection error.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// The talk does not exist or is not this teacher's.
    #[error("talk {0} is not one of this teacher's talks")]
    NotTheirTalk(TalkId),
    /// A wider level needs the teacher's recorded word.
    #[error(
        "{from} to {to} widens what is shown; give --evidence pointing to the teacher's own request"
    )]
    WideningNeedsEvidence {
        /// The current level.
        from: DisplayLevel,
        /// The requested level.
        to: DisplayLevel,
    },
    /// A talk can be set only to excluded, excerpts or owner only, and never wider than its
    /// teacher.
    #[error(
        "a talk can be set to a, b or d, no wider than its teacher's {teacher}; {wanted} is not allowed"
    )]
    TalkLevel {
        /// The teacher's level.
        teacher: DisplayLevel,
        /// What was asked.
        wanted: DisplayLevel,
    },
    /// Levels e and f run the takedown and have their own commands.
    #[error("{0} runs a takedown: use `wt teacher opt-out` for f, and the owner export for e")]
    Takedown(DisplayLevel),
    /// The stored value is not a level.
    #[error("unknown stored display level {0:?}")]
    Unknown(String),
}

/// What a change did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LevelChange {
    /// The teacher's display name.
    pub teacher: String,
    /// The talk, when the change was for one talk.
    pub talk: Option<TalkId>,
    /// Before.
    pub from: DisplayLevel,
    /// After.
    pub to: DisplayLevel,
    /// Whether anything changed.
    pub changed: bool,
}

fn level(value: &str) -> Result<DisplayLevel, DisplayError> {
    DisplayLevel::parse(value).ok_or_else(|| DisplayError::Unknown(value.to_owned()))
}

/// A teacher's level.
pub async fn teacher_level(
    conn: &mut PgConnection,
    teacher: &str,
) -> Result<(String, DisplayLevel), DisplayError> {
    let (id, name) = find_teacher(conn, teacher).await?;
    let value = sqlx::query_scalar!(
        "select display_level from teacher where id = $1",
        id.as_uuid()
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok((name, level(&value)?))
}

/// Sets a teacher's level, or with `talk` that talk's. Narrowing applies at once; widening
/// needs `evidence`, a pointer to the teacher's own request. Levels e and f are refused here:
/// they run the takedown through their own commands.
pub async fn set_level(
    conn: &mut PgConnection,
    teacher: &str,
    talk: Option<TalkId>,
    to: DisplayLevel,
    actor: &str,
    reason: &str,
    evidence: Option<&str>,
) -> Result<LevelChange, DisplayError> {
    if matches!(to, DisplayLevel::Removed | DisplayLevel::ExportThenDelete) {
        return Err(DisplayError::Takedown(to));
    }
    let mut tx = sqlx::Acquire::begin(&mut *conn).await?;
    let (id, name) = find_teacher(&mut tx, teacher).await?;
    let teacher_value = sqlx::query_scalar!(
        "select display_level from teacher where id = $1 for update",
        id.as_uuid()
    )
    .fetch_one(&mut *tx)
    .await?;
    let teacher_level = level(&teacher_value)?;

    let (from, entity, entity_id) = match talk {
        None => (teacher_level, "teacher", id.as_uuid()),
        Some(talk) => {
            let row = sqlx::query!(
                "select display_level from talk where id = $1 and teacher_id = $2 for update",
                talk.as_uuid(),
                id.as_uuid()
            )
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(DisplayError::NotTheirTalk(talk))?;
            if !matches!(
                to,
                DisplayLevel::Excluded | DisplayLevel::Excerpts | DisplayLevel::OwnerOnly
            ) || to > teacher_level
            {
                return Err(DisplayError::TalkLevel {
                    teacher: teacher_level,
                    wanted: to,
                });
            }
            let current = match row.display_level {
                Some(v) => level(&v)?.min(teacher_level),
                None => teacher_level,
            };
            (current, "talk", talk.as_uuid())
        }
    };
    if to > from && evidence.is_none() {
        return Err(DisplayError::WideningNeedsEvidence { from, to });
    }
    let changed = to != from;
    if changed {
        match talk {
            None => {
                sqlx::query!(
                    "update teacher set display_level = $2 where id = $1",
                    id.as_uuid(),
                    to.as_str()
                )
                .execute(&mut *tx)
                .await?;
            }
            Some(talk) => {
                sqlx::query!(
                    "update talk set display_level = $2 where id = $1",
                    talk.as_uuid(),
                    to.as_str()
                )
                .execute(&mut *tx)
                .await?;
            }
        }
        let why = match evidence {
            Some(e) => format!("{reason} (evidence: {e})"),
            None => reason.to_owned(),
        };
        log_change(
            &mut tx,
            entity,
            entity_id,
            "display_level",
            Some(serde_json::json!(from.as_str())),
            Some(serde_json::json!(to.as_str())),
            actor,
            &why,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(LevelChange {
        teacher: name,
        talk,
        from,
        to,
        changed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_order_narrowest_first_and_parse_letters() {
        assert!(DisplayLevel::Removed < DisplayLevel::Excluded);
        assert!(DisplayLevel::Excerpts < DisplayLevel::Full);
        assert_eq!(DisplayLevel::parse("c"), Some(DisplayLevel::Full));
        assert_eq!(
            DisplayLevel::parse("owner_only"),
            Some(DisplayLevel::OwnerOnly)
        );
        assert_eq!(DisplayLevel::parse("D"), Some(DisplayLevel::OwnerOnly));
        assert_eq!(DisplayLevel::parse("x"), None);
        for l in DisplayLevel::ALL {
            assert_eq!(DisplayLevel::parse(l.as_str()), Some(l));
            assert_eq!(DisplayLevel::parse(&l.letter().to_string()), Some(l));
        }
        assert_eq!(
            DisplayLevel::ALL.map(DisplayLevel::rank),
            [0, 1, 2, 3, 4, 5],
            "the database function ranks the same way"
        );
    }
}
