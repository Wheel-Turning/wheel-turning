//! Full-text and vector search (Spec 05, steps 1 and 2). Every query here reads
//! `servable_passage`, and the student-question search `servable_student_question`, and nothing
//! else (CLAUDE.md rule 3).

use sqlx::{Connection, PgConnection, PgPool};
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

/// Passages the full-text statistics must cover before common lexemes are left out: a small
/// collection's statistics say little about which words are common.
pub const COMMON_MIN_ROWS: i64 = 10_000;

/// The lexemes the full-text query keeps: those not in more than `max_share` of passages
/// (`share` is `None` for them), or, when every lexeme is that common, the least common one so
/// the search still runs. Order is kept.
pub fn keep_lexemes(lexemes: &[(String, Option<f32>)]) -> Vec<String> {
    let kept: Vec<String> = lexemes
        .iter()
        .filter(|(_, share)| share.is_none())
        .map(|(l, _)| l.clone())
        .collect();
    if !kept.is_empty() {
        return kept;
    }
    lexemes
        .iter()
        .min_by(|a, b| a.1.unwrap_or(0.0).total_cmp(&b.1.unwrap_or(0.0)))
        .map(|(l, _)| vec![l.clone()])
        .unwrap_or_default()
}

/// The kept lexemes as a `tsquery` in the `simple` configuration, which leaves stems as they are,
/// each quoted so no lexeme can read as an operator.
pub fn lexeme_query(lexemes: &[String]) -> String {
    lexemes
        .iter()
        .map(|l| format!("'{}'", l.replace('\\', "\\\\").replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Full-text candidates, best first. The query's words are stemmed as the passages were, and
/// lexemes in more than `max_share` of passages are left out (migration 0045, owner's request of
/// October 15, 2026): they matched most of the collection, cost over a second a question, and
/// crowded out passages that share the question's distinctive words. `max_share` of 1 or more
/// keeps every word.
pub async fn full_text(
    conn: &mut PgConnection,
    tsquery: &str,
    teacher: Option<TeacherId>,
    k: i64,
    max_share: f32,
) -> Result<Vec<PassageId>, sqlx::Error> {
    let lexemes: Vec<(String, Option<f32>)> = sqlx::query!(
        r#"select l.lexeme as "lexeme!", c.share
           from unnest(to_tsvector('english', $1)) l
           left join common_lexemes($2, $3) c using (lexeme)"#,
        tsquery,
        max_share,
        COMMON_MIN_ROWS
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|r| (r.lexeme, r.share))
    .collect();
    let kept = keep_lexemes(&lexemes);
    if kept.is_empty() {
        return Ok(Vec::new());
    }
    let query = lexeme_query(&kept);
    let rows = sqlx::query_scalar!(
        r#"select passage_id as "passage_id!" from servable_passage
           where tsv @@ to_tsquery('simple', $1)
             and ($2::uuid is null or teacher_id = $2)
           order by ts_rank_cd(tsv, to_tsquery('simple', $1)) desc, passage_id
           limit $3"#,
        query,
        teacher.map(|t| t.as_uuid()),
        k
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(PassageId::from).collect())
}

/// The SQL of the standard-mode vector search, public so a test can check its plan. It orders
/// by the half-precision cast that `passage_embedding_half_hnsw` indexes (migration 0043): a
/// third of the full index's size, so it stays in the database's cache, at the same recall.
pub const VECTOR_SQL: &str = "select passage_id from servable_passage
                              order by embedding::halfvec(1024) <=> $1::text::halfvec(1024)
                              limit $2";

/// Settings that put the standard-mode vector search on `passage_embedding_half_hnsw`, for the
/// current transaction only. The view's rights filter makes the planner expect a handful of
/// servable rows (16 against 136,520 on October 12, 2026), so left alone it joins the whole
/// view and sorts every served passage, 2 to 4 s. With sorting off it walks the index
/// instead, about 50 ms. The large cost that disabling sort adds would also cross
/// `jit_above_cost` and spend about 500 ms compiling, so JIT is off too. A strict-order
/// iterative scan keeps reading the index, in exact distance order, until `limit` passages
/// pass the view's filter, so passages that do not serve never starve the list.
pub async fn use_hnsw(conn: &mut PgConnection, ef_search: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "select set_config('hnsw.ef_search', $1, true),
                set_config('hnsw.iterative_scan', 'strict_order', true),
                set_config('enable_sort', 'off', true),
                set_config('jit', 'off', true)",
    )
    .bind(ef_search.clamp(1, 1000).to_string())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Vector candidates, nearest first. Standard mode walks the HNSW index over the whole view
/// (see [`use_hnsw`]); named mode filters to one teacher first, an exact scan over that
/// teacher's passages, so a teacher with few passages is never cut short by the index.
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
            // Its own transaction, or a savepoint inside the caller's, rolled back at the end
            // so the planner settings stop here: nothing is written.
            let mut tx = conn.begin().await?;
            use_hnsw(&mut tx, ef_search).await?;
            let rows = sqlx::query_scalar(VECTOR_SQL)
                .bind(literal)
                .bind(k)
                .fetch_all(&mut *tx)
                .await?;
            tx.rollback().await?;
            rows
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

/// How many passages a search covers: every servable passage, or the named teacher's. Only the
/// live progress display reads it (Spec 06, Live progress while a question runs).
pub async fn scope_size(pool: &PgPool, teacher: Option<TeacherId>) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"select count(*) as "n!" from servable_passage
           where $1::uuid is null or teacher_id = $1"#,
        teacher.map(|t| t.as_uuid())
    )
    .fetch_one(pool)
    .await
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
    fn common_lexemes_leave_the_query_unless_all_are_common() {
        let l = |s: &str, share: Option<f32>| (s.to_owned(), share);
        assert_eq!(
            keep_lexemes(&[l("know", Some(0.52)), l("anger", None), l("resent", None)]),
            ["anger", "resent"]
        );
        assert_eq!(
            keep_lexemes(&[l("know", Some(0.52)), l("feel", Some(0.17))]),
            ["feel"],
            "the least common stays when every word is common"
        );
        assert!(keep_lexemes(&[]).is_empty());
    }

    #[test]
    fn lexemes_are_quoted_for_tsquery() {
        let q = lexeme_query(&["anger".to_owned(), "o'neil".to_owned(), "a\\b".to_owned()]);
        assert_eq!(q, "'anger' | 'o''neil' | 'a\\\\b'");
    }

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
