//! Student questions for display (Spec 04, Student questions, step 5). Serving reads them only
//! through `servable_student_question`, and each is shown above the passage it stands for: the
//! first servable passage of its talk that starts at or after its answer, the same passage
//! retrieval maps it to. The owner's exports read the table, as they read the stored transcript.

use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::ids::{PassageId, TalkId};

use crate::DbError;

/// A question and the passage it is shown above.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShownQuestion {
    /// The passage it stands for.
    pub passage_id: PassageId,
    /// The edited text.
    pub text: String,
}

/// The questions shown above any of `passages`.
pub async fn for_passages(
    pool: &PgPool,
    passages: &[PassageId],
) -> Result<Vec<ShownQuestion>, DbError> {
    let ids: Vec<Uuid> = passages.iter().map(|p| p.as_uuid()).collect();
    let rows = sqlx::query!(
        r#"select p.passage_id as "passage_id!", q.text as "text!"
           from servable_student_question q
           join lateral (select sp.passage_id from servable_passage sp
                         where sp.talk_id = q.talk_id and sp.start_ms >= q.answer_start_ms
                         order by sp.start_ms, sp.ordinal limit 1) p on true
           where q.talk_id in (select talk_id from servable_passage where passage_id = any($1))
             and p.passage_id = any($1)
           order by q.answer_start_ms"#,
        &ids
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ShownQuestion {
            passage_id: PassageId::from(r.passage_id),
            text: r.text,
        })
        .collect())
}

/// What the transcript page marks before a talk's passages: the edited questions, and the
/// answers whose question has no stored edit, which keep "A student asked a question".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TalkQuestions {
    /// Questions and the passage each is shown above.
    pub shown: Vec<ShownQuestion>,
    /// Passages that start a teacher's answer with no question shown.
    pub asked: Vec<PassageId>,
}

/// The marks for one serving talk. An answer starts at a `qa_teacher` passage that follows a
/// `talk` passage or a pause longer than `gap_ms`.
pub async fn for_talk(pool: &PgPool, talk: TalkId, gap_ms: i32) -> Result<TalkQuestions, DbError> {
    let passages = sqlx::query!(
        r#"select passage_id as "passage_id!", segment_kind::text as "kind!",
                  start_ms as "start_ms!", end_ms as "end_ms!"
           from servable_passage where talk_id = $1 order by ordinal"#,
        talk.as_uuid()
    )
    .fetch_all(pool)
    .await?;
    let ids: Vec<PassageId> = passages
        .iter()
        .map(|p| PassageId::from(p.passage_id))
        .collect();
    let shown = for_passages(pool, &ids).await?;
    let mut asked = Vec::new();
    let mut previous: Option<(&str, i32)> = None;
    for p in &passages {
        let id = PassageId::from(p.passage_id);
        let starts_answer = p.kind == "qa_teacher"
            && previous.is_none_or(|(kind, end)| kind != "qa_teacher" || p.start_ms - end > gap_ms);
        if starts_answer && !shown.iter().any(|q| q.passage_id == id) {
            asked.push(id);
        }
        previous = Some((p.kind.as_str(), p.end_ms));
    }
    Ok(TalkQuestions { shown, asked })
}

/// A stored question, for the owner's exports and review files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredQuestion {
    /// Start of the student turn.
    pub start_ms: i32,
    /// Start of the teacher's answer.
    pub answer_start_ms: i32,
    /// The edited text.
    pub text: String,
}

/// A talk's stored questions in order, whatever its serving state: the owner's exports read the
/// stored transcript, since a teacher at display level d or e has nothing servable.
pub async fn stored(
    conn: &mut sqlx::PgConnection,
    talk: TalkId,
) -> Result<Vec<StoredQuestion>, DbError> {
    let rows = sqlx::query!(
        "select start_ms, answer_start_ms, text from student_question
         where talk_id = $1 order by answer_start_ms",
        talk.as_uuid()
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| StoredQuestion {
            start_ms: r.start_ms,
            answer_start_ms: r.answer_start_ms,
            text: r.text,
        })
        .collect())
}
