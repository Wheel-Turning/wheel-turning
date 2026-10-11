//! Full-text and vector search (Spec 05, steps 1 and 2). Every query here reads
//! `servable_passage`, and the student-question search `servable_student_question`, and nothing
//! else (CLAUDE.md rule 3).

use sqlx::PgConnection;
use uuid::Uuid;
use wt_core::ids::{PassageId, TalkId, TeacherId};

/// Words dropped from the full-text query because they carry no topic. Postgres drops its own
/// stopwords too; this list also removes question words, which practitioners' questions are full
/// of.
const STOPWORDS: &[&str] = &[
    "a", "about", "an", "and", "are", "as", "at", "be", "but", "by", "can", "could", "do", "does",
    "for", "from", "how", "i", "if", "in", "is", "it", "me", "my", "of", "on", "or", "say", "says",
    "should", "so", "that", "the", "their", "there", "they", "this", "to", "was", "we", "what",
    "when", "where", "which", "who", "why", "with", "would", "you", "your",
];

/// Step 1: the content words of the query, lowercased, joined with OR for `to_tsquery`. Only
/// letters and digits survive, so the result is always valid `tsquery` syntax. `None` when no
/// content word is left.
pub fn or_query(text: &str) -> Option<String> {
    let mut words: Vec<String> = Vec::new();
    for w in text.split(|c: char| !c.is_alphanumeric()) {
        let w = w.to_lowercase();
        if w.len() > 1 && !STOPWORDS.contains(&w.as_str()) && !words.contains(&w) {
            words.push(w);
        }
    }
    (!words.is_empty()).then(|| words.join(" | "))
}

/// A vector as pgvector's text form, `[x,y,...]`.
pub fn vector_literal(v: &[f32]) -> String {
    let parts: Vec<String> = v.iter().map(|x| x.to_string()).collect();
    format!("[{}]", parts.join(","))
}

/// Parses pgvector's text form.
pub fn parse_vector(text: &str) -> Option<Vec<f32>> {
    text.trim()
        .strip_prefix('[')?
        .strip_suffix(']')?
        .split(',')
        .map(|x| x.trim().parse::<f32>().ok())
        .collect()
}

/// Full-text candidates, best first.
pub async fn full_text(
    conn: &mut PgConnection,
    tsquery: &str,
    teacher: Option<TeacherId>,
    k: i64,
) -> Result<Vec<PassageId>, sqlx::Error> {
    let rows = sqlx::query_scalar!(
        r#"select passage_id as "passage_id!" from servable_passage
           where tsv @@ to_tsquery('english', $1)
             and ($2::uuid is null or teacher_id = $2)
           order by ts_rank_cd(tsv, to_tsquery('english', $1)) desc, passage_id
           limit $3"#,
        tsquery,
        teacher.map(|t| t.as_uuid()),
        k
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(PassageId::from).collect())
}

/// Vector candidates, nearest first. Standard mode orders the whole view by distance so the
/// HNSW index serves it; named mode filters to one teacher first, an exact scan over that
/// teacher's passages, so a teacher with few passages is not starved by the index's
/// post-filter.
pub async fn vector(
    conn: &mut PgConnection,
    embedding: &[f32],
    teacher: Option<TeacherId>,
    k: i64,
    ef_search: i64,
) -> Result<Vec<PassageId>, sqlx::Error> {
    let literal = vector_literal(embedding);
    let rows: Vec<Uuid> = match teacher {
        None => {
            // `set local` lasts until the end of the caller's transaction.
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "set local hnsw.ef_search = {}",
                ef_search.clamp(1, 1000)
            )))
            .execute(&mut *conn)
            .await?;
            sqlx::query_scalar!(
                r#"select passage_id as "passage_id!" from servable_passage
                   order by embedding <=> $1::text::vector
                   limit $2"#,
                literal,
                k
            )
            .fetch_all(&mut *conn)
            .await?
        }
        Some(t) => {
            sqlx::query_scalar!(
                r#"select passage_id as "passage_id!" from servable_passage
                   where teacher_id = $2
                   order by embedding <=> $1::text::vector
                   limit $3"#,
                literal,
                t.as_uuid(),
                k
            )
            .fetch_all(&mut *conn)
            .await?
        }
    };
    Ok(rows.into_iter().map(PassageId::from).collect())
}

/// The third list (Spec 04, Student questions, step 6): the student questions nearest the
/// query, each standing for the first servable passage of its talk that starts at or after the
/// question's answer start. A question is never returned on its own, and one whose answer has
/// no servable passage gives nothing. Nearest first, each passage once. Named mode searches
/// that teacher's questions only.
pub async fn question_vector(
    conn: &mut PgConnection,
    embedding: &[f32],
    teacher: Option<TeacherId>,
    k: i64,
) -> Result<Vec<PassageId>, sqlx::Error> {
    let literal = vector_literal(embedding);
    let rows: Vec<Uuid> = sqlx::query_scalar!(
        r#"select p.passage_id as "passage_id!"
           from (select q.talk_id, q.answer_start_ms, q.embedding <=> $1::text::vector as d
                 from servable_student_question q
                 where $2::uuid is null or q.teacher_id = $2
                 order by q.embedding <=> $1::text::vector
                 limit $3) q
           join lateral (select sp.passage_id from servable_passage sp
                         where sp.talk_id = q.talk_id and sp.start_ms >= q.answer_start_ms
                         order by sp.start_ms, sp.ordinal limit 1) p on true
           order by q.d"#,
        literal,
        teacher.map(|t| t.as_uuid()),
        k
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut out: Vec<PassageId> = Vec::with_capacity(rows.len());
    for id in rows.into_iter().map(PassageId::from) {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(out)
}

/// A candidate's text and the fields selection needs.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// The passage.
    pub id: PassageId,
    /// Its verbatim text, for the reranker.
    pub text: String,
    /// Its teacher.
    pub teacher_id: TeacherId,
    /// Its talk.
    pub talk_id: TalkId,
    /// Where it starts.
    pub start_ms: i32,
    /// Its stored embedding.
    pub embedding: Vec<f32>,
}

/// Loads candidates by id. A passage that stopped serving since the search is simply absent.
pub async fn candidates(
    conn: &mut PgConnection,
    ids: &[PassageId],
) -> Result<Vec<Candidate>, sqlx::Error> {
    let uuids: Vec<Uuid> = ids.iter().map(|p| p.as_uuid()).collect();
    let rows = sqlx::query!(
        r#"select passage_id as "passage_id!", text as "text!", teacher_id as "teacher_id!",
                  talk_id as "talk_id!", start_ms as "start_ms!", embedding::text as "embedding!"
           from servable_passage where passage_id = any($1)"#,
        &uuids
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(Candidate {
                id: PassageId::from(r.passage_id),
                text: r.text,
                teacher_id: TeacherId::from(r.teacher_id),
                talk_id: TalkId::from(r.talk_id),
                start_ms: r.start_ms,
                embedding: parse_vector(&r.embedding)?,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_or_query_keeps_content_words_only() {
        assert_eq!(
            or_query("How do I work with anger in my practice?").as_deref(),
            Some("work | anger | practice")
        );
        assert_eq!(or_query("what is it?"), None);
        assert_eq!(
            or_query("O'Brien's koan: mu & (zazen)").as_deref(),
            Some("brien | koan | mu | zazen")
        );
    }

    #[test]
    fn vectors_round_trip_through_text() {
        let v = vec![0.5, -0.25, 1.0];
        assert_eq!(parse_vector(&vector_literal(&v)).unwrap(), v);
        assert_eq!(parse_vector("[0.1, 0.2]").unwrap(), vec![0.1, 0.2]);
        assert!(parse_vector("0.1,0.2").is_none());
    }
}
