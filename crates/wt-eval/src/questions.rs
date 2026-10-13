//! Question files and sets (Spec 07, command 1). A set is loaded once per name and version and
//! never changes: loading the same questions again is a no-op, and loading different questions
//! under a loaded version is refused.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::types::Uuid;

use crate::EvalError;

/// The tag on a question the owner has not approved yet. Its results are reported apart and do
/// not count toward the verdicts.
pub const DRAFT_TAG: &str = "draft";

/// One question as written in the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    /// What a practitioner asks, in their words.
    pub text: String,
    /// Free text; the report groups by them.
    #[serde(default)]
    pub tags: Vec<String>,
}

impl Question {
    /// Whether the owner has yet to approve it.
    pub fn is_draft(&self) -> bool {
        is_draft(&self.tags)
    }
}

/// Whether a tag list marks a draft question.
pub fn is_draft(tags: &[String]) -> bool {
    tags.iter().any(|t| t == DRAFT_TAG)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    question: Vec<Question>,
}

/// Parses a question file. Each question must be one the ask path accepts.
pub fn parse(text: &str) -> Result<Vec<Question>, EvalError> {
    let file: File = toml::from_str(text).map_err(|e| EvalError::Questions(e.to_string()))?;
    if file.question.is_empty() {
        return Err(EvalError::Questions("no [[question]] entries".into()));
    }
    for (n, q) in file.question.iter().enumerate() {
        wt_retrieve::query::normalize(&q.text)
            .map_err(|e| EvalError::Questions(format!("question {}: {e}", n + 1)))?;
    }
    Ok(file.question)
}

/// A set by name and version, written `name@version`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetRef {
    /// The set's name.
    pub name: String,
    /// Its version.
    pub version: i32,
}

impl fmt::Display for SetRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}

impl FromStr for SetRef {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let (name, version) = s
            .rsplit_once('@')
            .ok_or_else(|| format!("{s:?} is not name@version"))?;
        let version = version
            .parse()
            .map_err(|_| format!("{s:?}: the version must be a number"))?;
        if name.trim().is_empty() {
            return Err(format!("{s:?}: the name is empty"));
        }
        Ok(SetRef {
            name: name.trim().to_owned(),
            version,
        })
    }
}

/// A loaded question with its place in the set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    /// Its row.
    pub id: Uuid,
    /// From 1, in file order.
    pub ordinal: i32,
    /// The question.
    pub question: Question,
}

/// Loads a set and returns its id and whether this call created it.
pub async fn load(
    pool: &PgPool,
    set: &SetRef,
    questions: &[Question],
) -> Result<(Uuid, bool), EvalError> {
    let mut tx = pool.begin().await?;
    let existing = sqlx::query_scalar!(
        "select id from eval_set where name = $1 and version = $2 for update",
        set.name,
        set.version
    )
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(id) = existing {
        let stored = questions_of(&mut tx, id).await?;
        let same = stored.len() == questions.len()
            && stored.iter().zip(questions).all(|(s, q)| s.question == *q);
        return if same {
            Ok((id, false))
        } else {
            Err(EvalError::SetChanged(set.clone()))
        };
    }
    let id = sqlx::query_scalar!(
        "insert into eval_set (name, version) values ($1, $2) returning id",
        set.name,
        set.version
    )
    .fetch_one(&mut *tx)
    .await?;
    for (n, q) in questions.iter().enumerate() {
        let ordinal = i32::try_from(n + 1).unwrap_or(i32::MAX);
        sqlx::query!(
            "insert into eval_question (set_id, ordinal, text, tags) values ($1, $2, $3, $4)",
            id,
            ordinal,
            q.text,
            &q.tags
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok((id, true))
}

/// The id of a loaded set.
pub async fn find(pool: &PgPool, set: &SetRef) -> Result<Uuid, EvalError> {
    sqlx::query_scalar!(
        "select id from eval_set where name = $1 and version = $2",
        set.name,
        set.version
    )
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| EvalError::UnknownSet(set.clone()))
}

/// A set's questions in order.
pub async fn questions_of(
    conn: &mut sqlx::PgConnection,
    set: Uuid,
) -> Result<Vec<Stored>, EvalError> {
    let rows = sqlx::query!(
        "select id, ordinal, text, tags from eval_question where set_id = $1 order by ordinal",
        set
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Stored {
            id: r.id,
            ordinal: r.ordinal,
            question: Question {
                text: r.text,
                tags: r.tags,
            },
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_refs_parse_name_at_version() {
        let s: SetRef = "practitioner@1".parse().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.name, "practitioner");
        assert_eq!(s.version, 1);
        assert_eq!(s.to_string(), "practitioner@1");
        for bad in ["practitioner", "@1", "x@one"] {
            assert!(bad.parse::<SetRef>().is_err(), "{bad}");
        }
    }

    #[test]
    fn files_parse_and_refuse_questions_the_ask_path_would_refuse() {
        let qs = parse(
            "[[question]]\ntext = \"How do I sit with anger?\"\ntags = [\"multi_voice\", \"draft\"]\n",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(qs.len(), 1);
        assert!(qs[0].is_draft());
        assert!(parse("[[question]]\ntext = \"hi\"\n").is_err());
        assert!(parse("").is_err());
        assert!(parse("[[question]]\ntext = \"How?\"\nweight = 2\n").is_err());
    }

    #[test]
    fn the_repository_question_files_parse() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval");
        for name in [
            "gate_a_questions.toml",
            "questions_v1.toml",
            "teacher_prep_v1.toml",
        ] {
            let text = std::fs::read_to_string(root.join(name)).unwrap_or_else(|e| panic!("{e}"));
            let qs = parse(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!qs.is_empty(), "{name}");
        }
    }

    #[test]
    fn the_practitioner_set_has_fifty_questions_with_the_prd_coverage() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval");
        let v1 =
            parse(&std::fs::read_to_string(root.join("questions_v1.toml")).unwrap_or_default())
                .unwrap_or_else(|e| panic!("{e}"));
        let gate =
            parse(&std::fs::read_to_string(root.join("gate_a_questions.toml")).unwrap_or_default())
                .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(v1.len(), 50);
        // The Gate A questions open the set unchanged, so their results compare across both.
        assert_eq!(&v1[..gate.len()], &gate[..]);
        // The owner approved questions 21 to 50 on October 13, 2026 (practitioner@2).
        assert!(v1.iter().all(|q| !q.is_draft()));
        let tagged = |t: &str| v1.iter().filter(|q| q.tags.iter().any(|x| x == t)).count();
        for tag in [
            "multi_voice",
            "single_teacher",
            "named_teacher",
            "grief",
            "practice",
        ] {
            assert!(tagged(tag) > 0, "no {tag} question");
        }
        assert!(
            v1.iter()
                .any(|q| q.tags.iter().any(|t| t == "named_teacher")
                    && q.text.contains("Norman Fischer")),
            "the PRD's thin-coverage query for Norman Fischer"
        );
    }

    #[test]
    fn the_teacher_prep_set_is_approved_and_tagged() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval");
        let qs =
            parse(&std::fs::read_to_string(root.join("teacher_prep_v1.toml")).unwrap_or_default())
                .unwrap_or_else(|e| panic!("{e}"));
        assert!((15..=20).contains(&qs.len()), "{}", qs.len());
        assert!(
            qs.iter()
                .all(|q| !q.is_draft() && q.tags.iter().any(|t| t == "teacher_prep"))
        );
    }
}
