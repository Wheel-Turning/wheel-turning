//! The index step (Spec 04, Chunking, embedding and indexing): passages from the corrected
//! words of each segment, citations mapped onto them, and one transaction that brings the talk's
//! passages up to date. A passage whose segment and content hash are unchanged keeps its id, so
//! answers, feedback and eval ratings that name it survive a reindex. [`plan`] and
//! [`match_stored`] are pure; [`write`] runs inside the caller's transaction, which also moves
//! the talk to `ready`.

use sqlx::PgConnection;
use sqlx::types::Uuid;
use wt_core::records::sha256_hex;
use wt_core::transcript::Word;

use crate::chunker::{ChunkSettings, Passage, chunk};

/// `passage.text` must be at least this many characters (the database check).
pub const MIN_PASSAGE_CHARS: usize = 40;
/// And at most this many.
pub const MAX_PASSAGE_CHARS: usize = 1800;

/// Why indexing failed.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// A passage is longer than the database allows: the chunk settings need a smaller
    /// `max_words` or `merge_cap_words`.
    #[error("passage {ordinal} has {chars} characters, over the limit of {MAX_PASSAGE_CHARS}")]
    TooLong {
        /// Its place in the talk.
        ordinal: i32,
        /// Its length.
        chars: usize,
    },
    /// The embedder returned the wrong number or length of vectors.
    #[error("embedding: {0}")]
    Embedding(String),
    /// A passage matched before embedding was gone when the write began, as after a takedown
    /// or a new transcript. The job is retried and matches again.
    #[error("passage {0} changed while the talk was being embedded")]
    Changed(Uuid),
    /// The database refused.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

/// One `talk` or `qa_teacher` segment: its corrected words, each with the range of stored
/// (uncorrected) words it came from.
#[derive(Debug, Clone)]
pub struct IndexSegment {
    /// `segment.id`.
    pub id: Uuid,
    /// `talk` or `qa_teacher`.
    pub kind: String,
    /// Corrected words and their stored-word ranges.
    pub words: Vec<(Word, (usize, usize))>,
}

/// A staged, resolved citation, at stored-word positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexCitation {
    /// The first stored word it covers.
    pub w0: usize,
    /// One past the last.
    pub w1: usize,
    /// `resolved`, `probable` or `unresolved`.
    pub confidence: String,
    /// `hand_list`, `series_label` or `llm`.
    pub method: String,
    /// The hand-listed locator, absent when unresolved.
    pub work_locator_id: Option<Uuid>,
}

/// A passage to insert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedPassage {
    /// Its segment.
    pub segment_id: Uuid,
    /// The segment's kind.
    pub kind: String,
    /// Its place in the talk, from 0.
    pub ordinal: i32,
    /// The chunk.
    pub passage: Passage,
}

/// A citation to insert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCitation {
    /// Index into [`Plan::passages`] of the passage holding its first word.
    pub passage: usize,
    /// Character offsets inside the passage text.
    pub span_start: i32,
    /// End offset, cut at the passage's end when the citation runs past it.
    pub span_end: i32,
    /// The citing words as they appear in the corrected text.
    pub raw_text: String,
    /// As staged.
    pub confidence: String,
    /// As staged.
    pub method: String,
    /// As staged.
    pub work_locator_id: Option<Uuid>,
}

/// A staged student question, lightly edited (Spec 04, Student questions), to store with its
/// vector beside the talk's passages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexQuestion {
    /// Start of the student turn.
    pub start_ms: i32,
    /// End of the student turn.
    pub end_ms: i32,
    /// Start of the teacher's answer.
    pub answer_start_ms: i32,
    /// The edited text, which is what is embedded.
    pub text: String,
    /// Its words.
    pub word_count: i32,
    /// SHA-256 of the text.
    pub content_hash: String,
    /// The model that edited it.
    pub edit_model: String,
}

/// What the index step will write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The passages, in order.
    pub passages: Vec<PlannedPassage>,
    /// The citations.
    pub citations: Vec<PlannedCitation>,
    /// Passages under [`MIN_PASSAGE_CHARS`], left out: a short answer such as "Yes, exactly."
    pub skipped_short: usize,
    /// Citations whose first word is in no kept passage.
    pub dropped_citations: usize,
    /// The talk's student questions, embedded after the passages.
    pub questions: Vec<IndexQuestion>,
    /// `talk.content_hash`: the passages, the citations and the embedder's vector ID (the
    /// model and how vectors are computed, `Embedder::vector_id`).
    pub content_hash: String,
}

/// The kept words of a passage, as the chunker joins them, with each word's character span.
fn word_spans(words: &[(Word, (usize, usize))], range: (usize, usize)) -> Vec<(usize, i32, i32)> {
    let mut out = Vec::new();
    let mut pos = 0i32;
    for (j, (w, _)) in words.iter().enumerate().take(range.1).skip(range.0) {
        let t = w.text.trim();
        if t.is_empty() {
            continue;
        }
        if !out.is_empty() {
            pos += 1;
        }
        let len = i32::try_from(t.chars().count()).unwrap_or(i32::MAX);
        out.push((j, pos, pos + len));
        pos += len;
    }
    out
}

/// Builds passages and maps citations onto them. Pure and deterministic. `vector_id` names
/// the model and how vectors are computed; it goes into the content hash.
pub fn plan(
    segments: &[IndexSegment],
    citations: &[IndexCitation],
    cfg: &ChunkSettings,
    vector_id: &str,
) -> Result<Plan, IndexError> {
    let mut passages = Vec::new();
    let mut skipped_short = 0;
    // For each kept passage: its segment's index.
    let mut seg_of = Vec::new();
    for (si, seg) in segments.iter().enumerate() {
        let words: Vec<Word> = seg.words.iter().map(|(w, _)| w.clone()).collect();
        for p in chunk(&words, cfg) {
            let chars = p.text.chars().count();
            if chars < MIN_PASSAGE_CHARS {
                skipped_short += 1;
                continue;
            }
            let ordinal = i32::try_from(passages.len()).unwrap_or(i32::MAX);
            if chars > MAX_PASSAGE_CHARS {
                return Err(IndexError::TooLong { ordinal, chars });
            }
            passages.push(PlannedPassage {
                segment_id: seg.id,
                kind: seg.kind.clone(),
                ordinal,
                passage: p,
            });
            seg_of.push(si);
        }
    }

    let mut planned = Vec::new();
    let mut dropped_citations = 0;
    for c in citations {
        // The corrected word holding the citation's first stored word, and its passage.
        let found = passages.iter().enumerate().find_map(|(pi, p)| {
            let words = &segments[seg_of[pi]].words;
            let (a, b) = p.passage.words;
            words[a..b]
                .iter()
                .position(|(_, (x, y))| *x <= c.w0 && c.w0 < *y)
                .map(|_| pi)
        });
        let Some(pi) = found else {
            dropped_citations += 1;
            continue;
        };
        let words = &segments[seg_of[pi]].words;
        let spans = word_spans(words, passages[pi].passage.words);
        let covers = |j: usize| {
            let (x, y) = words[j].1;
            x < c.w1 && c.w0 < y
        };
        let inside: Vec<&(usize, i32, i32)> = spans.iter().filter(|(j, _, _)| covers(*j)).collect();
        let (Some(first), Some(last)) = (inside.first(), inside.last()) else {
            dropped_citations += 1;
            continue;
        };
        let raw_text = segments[seg_of[pi]]
            .words
            .iter()
            .filter(|(_, (x, y))| *x < c.w1 && c.w0 < *y)
            .map(|(w, _)| w.text.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        planned.push(PlannedCitation {
            passage: pi,
            span_start: first.1,
            span_end: last.2,
            raw_text,
            confidence: c.confidence.clone(),
            method: c.method.clone(),
            work_locator_id: c.work_locator_id,
        });
    }

    let mut h = format!("model\u{1f}{vector_id}\u{1e}");
    for p in &passages {
        // The sentence boundaries go in too, so a talk indexed before they were stored is
        // indexed again; they never touch the passage's own hash, which keeps its id.
        h.push_str(&format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{:?}\u{1f}{:?}\u{1e}",
            p.segment_id,
            p.kind,
            p.passage.content_hash,
            p.passage.sentence_starts,
            p.passage.sentence_start_ms
        ));
    }
    for c in &planned {
        h.push_str(&format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{:?}\u{1e}",
            c.passage, c.span_start, c.span_end, c.confidence, c.method, c.work_locator_id
        ));
    }
    Ok(Plan {
        passages,
        citations: planned,
        skipped_short,
        dropped_citations,
        questions: Vec::new(),
        content_hash: sha256_hex(h.as_bytes()),
    })
}

impl Plan {
    /// Adds the talk's student questions. With any, they join the content hash, so a talk
    /// whose questions change is indexed again; with none, the hash is as before.
    pub fn with_questions(mut self, questions: Vec<IndexQuestion>) -> Self {
        if !questions.is_empty() {
            let mut h = format!("{}\u{1e}", self.content_hash);
            for q in &questions {
                h.push_str(&format!(
                    "q\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1e}",
                    q.start_ms, q.end_ms, q.answer_start_ms, q.content_hash
                ));
            }
            self.content_hash = sha256_hex(h.as_bytes());
        }
        self.questions = questions;
        self
    }

    /// The texts to embed, in the order [`write`] takes their vectors: the passages whose
    /// stored vector cannot be kept, then the questions.
    pub fn texts(&self, reuse: &Reuse) -> Vec<String> {
        self.passages
            .iter()
            .zip(&reuse.keep_vector)
            .filter(|(_, keep)| !**keep)
            .map(|(p, _)| p.passage.text.clone())
            .chain(self.questions.iter().map(|q| q.text.clone()))
            .collect()
    }
}

/// A passage the talk already has, as matching sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPassage {
    /// `passage.id`.
    pub id: Uuid,
    /// Its segment.
    pub segment_id: Uuid,
    /// Its chunk's hash, over the text and offsets.
    pub content_hash: String,
    /// Whether its vector came from the embedder's model and vector version.
    pub vector_current: bool,
}

/// For each planned passage, in order: the stored passage it keeps, and whether that passage's
/// vector can stay.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reuse {
    /// The stored passage's id, or none for a new passage.
    pub ids: Vec<Option<Uuid>>,
    /// True when the stored vector is current, so the passage needs no embedding.
    pub keep_vector: Vec<bool>,
}

/// Matches planned passages to stored ones by segment and content hash, which covers the text
/// and its offsets. Each stored passage is used at most once. Pure.
pub fn match_stored(plan: &Plan, stored: &[StoredPassage]) -> Reuse {
    let mut used = vec![false; stored.len()];
    let mut reuse = Reuse::default();
    for p in &plan.passages {
        let found = stored.iter().enumerate().position(|(i, s)| {
            !used[i] && s.segment_id == p.segment_id && s.content_hash == p.passage.content_hash
        });
        match found {
            Some(i) => {
                used[i] = true;
                reuse.ids.push(Some(stored[i].id));
                reuse.keep_vector.push(stored[i].vector_current);
            }
            None => {
                reuse.ids.push(None);
                reuse.keep_vector.push(false);
            }
        }
    }
    reuse
}

/// Reads the talk's stored passages and matches the plan to them ([`match_stored`]). A stored
/// vector is current when it came from `model_id` at `vector_version`.
pub async fn reuse(
    conn: &mut PgConnection,
    talk: Uuid,
    plan: &Plan,
    model_id: &str,
    vector_version: i32,
) -> Result<Reuse, IndexError> {
    let stored: Vec<StoredPassage> = sqlx::query!(
        r#"select id, segment_id, content_hash,
                  (embedding_model = $2 and vector_version = $3) as "vector_current!"
           from passage where talk_id = $1 order by ordinal"#,
        talk,
        model_id,
        vector_version
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|r| StoredPassage {
        id: r.id,
        segment_id: r.segment_id,
        content_hash: r.content_hash,
        vector_current: r.vector_current,
    })
    .collect();
    Ok(match_stored(plan, &stored))
}

/// Whether the talk is `ready` with exactly this content hash, so indexing again would change
/// nothing.
pub async fn is_current(
    conn: &mut PgConnection,
    talk: Uuid,
    content_hash: &str,
) -> Result<bool, IndexError> {
    Ok(sqlx::query_scalar!(
        r#"select exists (select 1 from talk where id = $1 and state = 'ready'
                          and content_hash = $2) as "e!""#,
        talk,
        content_hash
    )
    .fetch_one(conn)
    .await?)
}

fn vector_literal(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 10 + 2);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&x.to_string());
    }
    s.push(']');
    s
}

/// Brings the talk's passages, citations and student questions up to date and records the
/// content hash. A passage `reuse` matched keeps its id and is updated in place, with a new
/// vector only when its stored one is stale; a new chunk is inserted; a stored passage that
/// nothing matched is deleted, which cascades to its citations and answers' rows. `vectors` are
/// those [`Plan::texts`] lists for the same `reuse`. Run it inside the transaction that moves
/// the talk to `ready`; the caller commits.
#[allow(clippy::too_many_arguments)]
pub async fn write(
    conn: &mut PgConnection,
    talk: Uuid,
    plan: &Plan,
    reuse: &Reuse,
    vectors: &[Vec<f32>],
    model_id: &str,
    vector_version: i32,
    dim: usize,
) -> Result<(), IndexError> {
    if reuse.ids.len() != plan.passages.len() || reuse.keep_vector.len() != plan.passages.len() {
        return Err(IndexError::Embedding(format!(
            "a match for {} passages, not {}",
            reuse.ids.len(),
            plan.passages.len()
        )));
    }
    let to_embed = reuse.keep_vector.iter().filter(|keep| !**keep).count();
    if vectors.len() != to_embed + plan.questions.len() || vectors.iter().any(|v| v.len() != dim) {
        return Err(IndexError::Embedding(format!(
            "{} vectors for {} passages and {} questions",
            vectors.len(),
            to_embed,
            plan.questions.len()
        )));
    }
    let (vectors, question_vectors) = vectors.split_at(to_embed);
    let mut fresh = vectors.iter();
    // Per planned passage: its new vector, or none to keep the stored one.
    let embeddings: Vec<Option<String>> = reuse
        .keep_vector
        .iter()
        .map(|keep| {
            if *keep {
                None
            } else {
                fresh.next().map(|v| vector_literal(v))
            }
        })
        .collect();

    // The stored passages, locked: one matched before embedding may have gone since.
    let present: Vec<Uuid> =
        sqlx::query_scalar!("select id from passage where talk_id = $1 for update", talk)
            .fetch_all(&mut *conn)
            .await?;
    let kept: Vec<Uuid> = reuse.ids.iter().flatten().copied().collect();
    if let Some(gone) = kept.iter().find(|id| !present.contains(id)) {
        return Err(IndexError::Changed(*gone));
    }
    // Passages that disappeared go, cascading to their citations and answers' rows. The kept
    // passages' citations are written again below.
    sqlx::query!(
        "delete from passage where talk_id = $1 and id <> all($2)",
        talk,
        &kept
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!("delete from citation where talk_id = $1", talk)
        .execute(&mut *conn)
        .await?;
    // Moves the kept passages out of the way of unique (talk_id, ordinal), which is checked row
    // by row, so that ordinals can shift.
    sqlx::query!(
        "update passage set ordinal = -1 - ordinal where talk_id = $1",
        talk
    )
    .execute(&mut *conn)
    .await?;

    let ids: Vec<Uuid> = reuse
        .ids
        .iter()
        .map(|id| id.unwrap_or_else(Uuid::new_v4))
        .collect();
    let is_new: Vec<bool> = reuse.ids.iter().map(Option::is_none).collect();
    let segs: Vec<Uuid> = plan.passages.iter().map(|p| p.segment_id).collect();
    let kinds: Vec<String> = plan.passages.iter().map(|p| p.kind.clone()).collect();
    let ordinals: Vec<i32> = plan.passages.iter().map(|p| p.ordinal).collect();
    let starts: Vec<i32> = plan.passages.iter().map(|p| p.passage.start_ms).collect();
    let ends: Vec<i32> = plan.passages.iter().map(|p| p.passage.end_ms).collect();
    let texts: Vec<String> = plan
        .passages
        .iter()
        .map(|p| p.passage.text.clone())
        .collect();
    let counts: Vec<i32> = plan
        .passages
        .iter()
        .map(|p| i32::try_from(p.passage.word_count).unwrap_or(i32::MAX))
        .collect();
    let hashes: Vec<String> = plan
        .passages
        .iter()
        .map(|p| p.passage.content_hash.clone())
        .collect();
    // Kept passages: everything but the id, and the vector only where it was stale.
    sqlx::query!(
        "update passage p set segment_id = u.seg, segment_kind = u.kind::segment_kind,
                ordinal = u.ord, start_ms = u.s, end_ms = u.e, text = u.txt, word_count = u.wc,
                content_hash = u.h, embedding = coalesce(u.emb::vector, p.embedding),
                embedding_model = case when u.emb is null then p.embedding_model else $12 end,
                vector_version = case when u.emb is null then p.vector_version else $13 end
         from unnest($2::uuid[], $3::uuid[], $4::text[], $5::int4[], $6::int4[], $7::int4[],
                     $8::text[], $9::int4[], $10::text[], $11::text[], $14::bool[])
              as u(id, seg, kind, ord, s, e, txt, wc, h, emb, new)
         where p.talk_id = $1 and p.id = u.id and not u.new",
        talk,
        &ids,
        &segs,
        &kinds,
        &ordinals,
        &starts,
        &ends,
        &texts,
        &counts,
        &hashes,
        &embeddings as &[Option<String>],
        model_id,
        vector_version,
        &is_new
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "insert into passage (id, talk_id, segment_id, segment_kind, ordinal, start_ms, end_ms,
                              text, word_count, content_hash, embedding, embedding_model,
                              vector_version)
         select u.id, $1, u.seg, u.kind::segment_kind, u.ord, u.s, u.e, u.txt, u.wc, u.h,
                u.emb::vector, $12, $13
         from unnest($2::uuid[], $3::uuid[], $4::text[], $5::int4[], $6::int4[], $7::int4[],
                     $8::text[], $9::int4[], $10::text[], $11::text[], $14::bool[])
              as u(id, seg, kind, ord, s, e, txt, wc, h, emb, new)
         where u.new",
        talk,
        &ids,
        &segs,
        &kinds,
        &ordinals,
        &starts,
        &ends,
        &texts,
        &counts,
        &hashes,
        &embeddings as &[Option<String>],
        model_id,
        vector_version,
        &is_new
    )
    .execute(&mut *conn)
    .await?;
    // Sentence boundaries for answer excerpts (Spec 04), as array literals since each passage
    // has its own number of them.
    let literal = |v: &[i32]| {
        let items: Vec<String> = v.iter().map(i32::to_string).collect();
        format!("{{{}}}", items.join(","))
    };
    let sentence_starts: Vec<String> = plan
        .passages
        .iter()
        .map(|p| literal(&p.passage.sentence_starts))
        .collect();
    let sentence_ms: Vec<String> = plan
        .passages
        .iter()
        .map(|p| literal(&p.passage.sentence_start_ms))
        .collect();
    sqlx::query!(
        "update passage p set sentence_starts = u.ss::int4[], sentence_start_ms = u.sm::int4[]
         from unnest($2::uuid[], $3::text[], $4::text[]) as u(id, ss, sm)
         where p.talk_id = $1 and p.id = u.id",
        talk,
        &ids,
        &sentence_starts,
        &sentence_ms
    )
    .execute(&mut *conn)
    .await?;

    let c_passages: Vec<Uuid> = plan.citations.iter().map(|c| ids[c.passage]).collect();
    let c_starts: Vec<i32> = plan.citations.iter().map(|c| c.span_start).collect();
    let c_ends: Vec<i32> = plan.citations.iter().map(|c| c.span_end).collect();
    let c_raw: Vec<String> = plan.citations.iter().map(|c| c.raw_text.clone()).collect();
    let c_conf: Vec<String> = plan
        .citations
        .iter()
        .map(|c| c.confidence.clone())
        .collect();
    let c_method: Vec<String> = plan.citations.iter().map(|c| c.method.clone()).collect();
    let c_locator: Vec<Option<Uuid>> = plan.citations.iter().map(|c| c.work_locator_id).collect();
    sqlx::query!(
        "insert into citation (passage_id, talk_id, span_start, span_end, raw_text,
                               work_locator_id, confidence, method)
         select u.p, $1, u.s, u.e, u.raw, u.loc, u.conf::confidence, u.m
         from unnest($2::uuid[], $3::int4[], $4::int4[], $5::text[], $6::uuid[], $7::text[],
                     $8::text[]) as u(p, s, e, raw, loc, conf, m)",
        talk,
        &c_passages,
        &c_starts,
        &c_ends,
        &c_raw,
        &c_locator as &[Option<Uuid>],
        &c_conf,
        &c_method
    )
    .execute(&mut *conn)
    .await?;

    // A citation whose locator has a unit in a ready, excerptable edition links to it, so its
    // footnote can open the text (Spec 08, Import pipeline, step 7).
    sqlx::query!(
        "update citation c set text_unit_id = u.id
         from work_locator wl
         join text_edition e on e.work_id = wl.work_id
         join text_unit u on u.edition_id = e.id and u.locator = wl.locator
         where c.talk_id = $1 and c.work_locator_id = wl.id
           and e.state = 'ready' and e.excerptable",
        talk
    )
    .execute(&mut *conn)
    .await?;

    // Student questions belong to the current transcript; a talk without any keeps none.
    sqlx::query!("delete from student_question where talk_id = $1", talk)
        .execute(&mut *conn)
        .await?;
    for (q, v) in plan.questions.iter().zip(question_vectors) {
        sqlx::query!(
            "insert into student_question (talk_id, transcript_id, start_ms, end_ms,
                                           answer_start_ms, text, word_count, content_hash,
                                           embedding, embedding_model, edit_model)
             select $1, tr.id, $2, $3, $4, $5, $6, $7, $8::text::vector, $9, $10
             from transcript tr where tr.talk_id = $1 and tr.is_current",
            talk,
            q.start_ms,
            q.end_ms,
            q.answer_start_ms,
            q.text,
            q.word_count,
            q.content_hash,
            vector_literal(v),
            model_id,
            q.edit_model
        )
        .execute(&mut *conn)
        .await?;
    }

    sqlx::query!(
        "update talk set content_hash = $2, last_verified_at = now() where id = $1",
        talk,
        plan.content_hash
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// `ANALYZE passage`, after a batch of talks (Spec 04).
pub async fn analyze(conn: &mut PgConnection) -> Result<(), IndexError> {
    sqlx::query!("analyze passage").execute(conn).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn seg(text: &str, offset: usize) -> IndexSegment {
        IndexSegment {
            id: Uuid::nil(),
            kind: "talk".into(),
            words: text
                .split_whitespace()
                .enumerate()
                .map(|(i, w)| {
                    let ms = i32::try_from(i).unwrap() * 300;
                    (
                        Word::new(w, ms, ms + 250, 0.9),
                        (offset + i, offset + i + 1),
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn a_citation_maps_to_character_offsets_in_its_passage() {
        let s = seg(
            "Dogen says to study the way is to study the self and we sit with that together.",
            0,
        );
        let c = IndexCitation {
            w0: 3,
            w1: 11,
            confidence: "resolved".into(),
            method: "hand_list".into(),
            work_locator_id: None,
        };
        let p = plan(&[s], &[c], &ChunkSettings::default(), "fake").unwrap();
        assert_eq!(p.passages.len(), 1);
        let pc = &p.citations[0];
        let text = &p.passages[0].passage.text;
        let span: String = text
            .chars()
            .skip(usize::try_from(pc.span_start).unwrap())
            .take(usize::try_from(pc.span_end - pc.span_start).unwrap())
            .collect();
        assert_eq!(span, "study the way is to study the self");
        assert_eq!(pc.raw_text, span);
    }

    #[test]
    fn short_passages_are_skipped_and_their_citations_dropped() {
        let short = seg("Yes, exactly.", 0);
        let c = IndexCitation {
            w0: 0,
            w1: 1,
            confidence: "unresolved".into(),
            method: "llm".into(),
            work_locator_id: None,
        };
        let p = plan(&[short], &[c], &ChunkSettings::default(), "fake").unwrap();
        assert_eq!((p.passages.len(), p.skipped_short), (0, 1));
        assert_eq!((p.citations.len(), p.dropped_citations), (0, 1));
    }

    #[test]
    fn the_content_hash_depends_on_the_model_and_nothing_random() {
        let s = seg(
            "A passage long enough to keep, about sitting still in the morning light.",
            0,
        );
        let a = plan(
            std::slice::from_ref(&s),
            &[],
            &ChunkSettings::default(),
            "m1",
        )
        .unwrap();
        let b = plan(
            std::slice::from_ref(&s),
            &[],
            &ChunkSettings::default(),
            "m1",
        )
        .unwrap();
        let c = plan(&[s], &[], &ChunkSettings::default(), "m2").unwrap();
        assert_eq!(a, b);
        assert_ne!(a.content_hash, c.content_hash);
    }
}
